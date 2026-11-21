// The tool reports errors by exit status from where they occur.
#![allow(clippy::exit)]

use mvdz::Config;
use mvdz::Limits;
use std::io::{Read, Write};

/// `println!` that exits quietly when standard output is closed (a pipe
/// into `head`, say) instead of panicking.
macro_rules! out {
    ($($t:tt)*) => {
        writeln!(std::io::stdout().lock(), $($t)*).unwrap_or_else(|e| stdout_failed(e))
    };
}

fn usage() -> ! {
    eprintln!(
        "usage:
  mvdz compress   [-j threads] [--allow-non-demo]
                  [-f] [--rm] files... | in out.mvdz | -o out.mvdz in
                  game.mvd -> game.mvdz beside it
  mvdz decompress [-j threads] [--max-memory size]
                  [-f] [--rm] files... | in.mvdz out | -o out in.mvdz
                  game.mvdz -> game.mvd beside it
  mvdz info       file.mvdz...                     header and segments as JSON
'-' as a file name is standard input or output. compress and decompress name
each output after its input, keep the inputs (--rm deletes each one after its
output is written), refuse to overwrite an existing file (-f overwrites), and
with several files work on -j of them at once; two names where the second is
an output (compress: *.mvdz or -; decompress: not *.mvdz) are input and
output, as is -o. compress takes a raw demo: input that is not an MVD demo
is refused (--allow-non-demo stores it anyway), and a .mvd.gz is to be
decompressed first (gzip -dc game.mvd.gz | mvdz compress - game.mvdz).
Sizes take K, M, G suffixes (binary: 1M = 1048576). Compression has no settings: the demo is cut into
independently decodable segments of 256 KiB. decompress decodes up to -j
segments at once, each with memory of its own: a tight --max-memory may need
a lower -j."
    );
    std::process::exit(2)
}

struct Args {
    threads: usize,
    max_memory: Option<u64>,
    files: Vec<String>,
    any_input: bool,
    output: Option<String>,
    overwrite: bool,
    rm: bool,
}

fn size(s: &str) -> u64 {
    let (num, mul) = match s.as_bytes().last() {
        Some(b'k' | b'K') => (&s[..s.len() - 1], 1u64 << 10),
        Some(b'm' | b'M') => (&s[..s.len() - 1], 1 << 20),
        Some(b'g' | b'G') => (&s[..s.len() - 1], 1 << 30),
        _ => (s, 1),
    };
    num.parse::<u64>()
        .ok()
        .and_then(|n| n.checked_mul(mul))
        .unwrap_or_else(|| usage())
}

fn parse(args: &[String]) -> Args {
    let mut a = Args {
        threads: std::thread::available_parallelism().map_or(1, std::num::NonZero::get),
        max_memory: None,
        files: Vec::new(),
        any_input: false,
        output: None,
        overwrite: false,
        rm: false,
    };
    let mut i = 0;
    while i < args.len() {
        let s = &args[i];
        let mut val = || {
            i += 1;
            args.get(i).cloned().unwrap_or_else(|| usage())
        };
        match s.as_str() {
            "-j" | "--threads" => {
                a.threads = mvdz::util::threads(val().parse().unwrap_or_else(|_| usage()));
            }
            "--max-memory" => a.max_memory = Some(size(&val())),
            "--allow-non-demo" => a.any_input = true,
            "-o" | "--output" => a.output = Some(val()),
            "-f" | "--force" => a.overwrite = true,
            "--rm" => a.rm = true,
            "-" => a.files.push(s.clone()),
            _ if s.starts_with('-') => usage(),
            _ => a.files.push(s.clone()),
        }
        i += 1;
    }
    a
}

/// A failed write to standard output: a closed pipe ends the program
/// quietly (the reader wanted no more), anything else is an error.
fn stdout_failed(e: std::io::Error) -> ! {
    if e.kind() == std::io::ErrorKind::BrokenPipe {
        std::process::exit(0)
    }
    die(e)
}

fn die(e: impl std::fmt::Display) -> ! {
    eprintln!("error: {e}");
    std::process::exit(1)
}

/// Reads a whole input file (or standard input), once.
fn try_read_input(p: &str) -> Result<Vec<u8>, String> {
    if p == "-" {
        let mut b = Vec::new();
        std::io::stdin()
            .lock()
            .read_to_end(&mut b)
            .map_err(|e| e.to_string())?;
        Ok(b)
    } else {
        std::fs::read(p).map_err(|e| e.to_string())
    }
}

/// Opens an input file (or standard input) to read in order.
fn open_input(p: &str) -> Result<Box<dyn Read>, String> {
    if p == "-" {
        Ok(Box::new(std::io::stdin().lock()))
    } else {
        let f = std::fs::File::open(p).map_err(|e| e.to_string())?;
        Ok(Box::new(std::io::BufReader::new(f)))
    }
}

/// Writes `data` as an output.
fn try_write_output(p: &str, data: &[u8]) -> Result<(), String> {
    try_write_with(p, |w| {
        w.write_all(data)?;
        w.flush()
    })
}

/// Writes an output with `fill`: standard output, a device or pipe
/// directly, a regular file atomically (temporary file and rename: when
/// `fill` fails, no file is left).
fn try_write_with(
    p: &str,
    fill: impl FnOnce(&mut dyn Write) -> std::io::Result<()>,
) -> Result<(), String> {
    if p == "-" {
        fill(&mut std::io::stdout().lock()).unwrap_or_else(|e| stdout_failed(e));
        return Ok(());
    }
    let path = std::path::Path::new(p);
    match std::fs::metadata(path) {
        Ok(m) if !m.is_file() => {
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .open(path)
                .map_err(|e| e.to_string())?;
            fill(&mut f).map_err(|e| e.to_string())
        }
        _ => mvdz::write_file_atomic_with(path, |f| fill(f)).map_err(|e| e.to_string()),
    }
}

/// Whether `name` ends with the ASCII suffix `suf`, ignoring case.
fn has_suffix(name: &str, suf: &str) -> bool {
    name.len() >= suf.len()
        && name.as_bytes()[name.len() - suf.len()..].eq_ignore_ascii_case(suf.as_bytes())
}

/// The output named after input `i`: game.mvd -> game.mvdz; game.mvdz ->
/// game.mvd.
fn output_name(i: &str, compress: bool) -> Result<String, String> {
    if i == "-" {
        return Ok("-".into());
    }
    if compress {
        if has_suffix(i, ".mvdz") {
            return Err("already .mvdz".into());
        }
        let stem = if has_suffix(i, ".mvd") {
            &i[..i.len() - 4]
        } else {
            i
        };
        Ok(format!("{stem}.mvdz"))
    } else if has_suffix(i, ".mvdz") {
        Ok(format!("{}.mvd", &i[..i.len() - 5]))
    } else {
        Err("not named *.mvdz (-o names the output)".into())
    }
}

/// The (input, output) pairs of a compress or decompress command line.
fn jobs(a: &Args, compress: bool) -> Vec<(String, Result<String, String>)> {
    let f = &a.files;
    if let Some(o) = &a.output {
        if f.len() != 1 {
            usage();
        }
        return vec![(f[0].clone(), Ok(o.clone()))];
    }
    if f.is_empty() {
        usage();
    }
    // the two-name form `in out`: the second names an output
    let second_is_output = |o: &str| o == "-" || has_suffix(o, ".mvdz") == compress;
    if f.len() == 2 && second_is_output(&f[1]) {
        return vec![(f[0].clone(), Ok(f[1].clone()))];
    }
    if f.len() > 1 && f.iter().any(|x| x == "-") {
        usage();
    }
    let mut seen = std::collections::HashMap::new();
    f.iter()
        .map(|i| {
            let o =
                output_name(i, compress).and_then(|o| match seen.insert(o.clone(), i.clone()) {
                    Some(prev) => Err(format!("{o} is also the output of {prev}")),
                    None => Ok(o),
                });
            (i.clone(), o)
        })
        .collect()
}

/// Runs every job of a compress or decompress command line: several files
/// on -j threads, one file at a time each. Errors are reported per file;
/// the exit status is 1 if any file failed.
fn run_jobs(a: &Args, compress: bool) {
    let jobs = jobs(a, compress);
    let (workers, threads) = if jobs.len() > 1 {
        (a.threads, 1)
    } else {
        (1, a.threads)
    };
    let failed = mvdz::util::par_map(workers, jobs.len(), |k| {
        let (i, o) = &jobs[k];
        let r = o.clone().and_then(|o| {
            if o != "-" && !a.overwrite && std::fs::metadata(&o).is_ok_and(|m| m.is_file()) {
                return Err(format!("{o} exists (-f overwrites)"));
            }
            if compress {
                compress_one(a, i, &o, threads)
            } else {
                decompress_one(a, i, &o, threads)
            }?;
            if a.rm && i != "-" && o != "-" {
                std::fs::remove_file(i).map_err(|e| format!("--rm: {e}"))?;
            }
            Ok(())
        });
        if let Err(e) = &r {
            eprintln!("error: {i}: {e}");
        }
        r.is_err()
    });
    if failed.contains(&true) {
        std::process::exit(1);
    }
}

fn compress_one(a: &Args, i: &str, o: &str, threads: usize) -> Result<(), String> {
    // non-demos are refused by the library
    let src = try_read_input(i)?;
    let cfg = Config { threads };
    let r = if a.any_input {
        // store whatever it is (it still round-trips)
        mvdz::encode::compress(&src, &cfg)
    } else {
        mvdz::compress_info(&src, &cfg)
    };
    let (out, _) = r.map_err(|e| match e {
        mvdz::Error::NotMvd(s) if !s.starts_with("gzip") => {
            format!("{e} (--allow-non-demo compresses it anyway, mostly stored)")
        }
        _ => e.to_string(),
    })?;
    try_write_output(o, &out).map_err(|e| format!("{o}: {e}"))
}

fn decompress_one(a: &Args, i: &str, o: &str, threads: usize) -> Result<(), String> {
    // decoded and written segment by segment: neither the file nor the
    // demo is held whole
    let input = open_input(i)?;
    let limits = read_limits(a);
    try_write_with(o, |w| {
        mvdz::decompress_stream(input, w, threads, &limits)?;
        Ok(())
    })
}

fn read_limits(a: &Args) -> Limits {
    let mut l = Limits::default();
    if let Some(m) = a.max_memory {
        l.memory = m;
    }
    l
}

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    if argv.is_empty() {
        usage();
    }
    let a = parse(&argv[1..]);
    match argv[0].as_str() {
        "compress" | "c" => run_jobs(&a, true),
        "decompress" | "d" => run_jobs(&a, false),
        "info" => info(&a),
        _ => usage(),
    }
}

fn info(a: &Args) {
    for f in &a.files {
        let r = std::fs::File::open(f).unwrap_or_else(|e| die(e));
        match mvdz::read_info(std::io::BufReader::new(r)) {
            Ok(fi) => out!("{}", fi.to_json()),
            Err(e) => die(format!("{f}: {e}")),
        }
    }
}
