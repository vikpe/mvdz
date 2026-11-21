//! The `mvdz` command-line tool.

use mvdz::Config;
use mvdz::Limits;
use std::io::{Read, Write};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(code) => code,
    }
}

/// Runs the command line; `Err` carries the exit status to stop with.
fn run() -> Result<(), ExitCode> {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let Some((command, rest)) = argv.split_first() else {
        return Err(usage());
    };
    let args = parse(rest)?;
    match command.as_str() {
        "compress" | "c" => run_jobs(&args, true),
        "decompress" | "d" => run_jobs(&args, false),
        "info" => info(&args),
        _ => Err(usage()),
    }
}

/// Prints the usage; the exit status of a bad command line.
fn usage() -> ExitCode {
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
    ExitCode::from(2)
}

struct Args {
    threads: usize,
    max_memory: Option<u64>,
    files: Vec<String>,
    is_any_input_allowed: bool,
    output: Option<String>,
    should_overwrite: bool,
    should_remove_inputs: bool,
}

/// A size with an optional K, M or G suffix (binary).
fn size(text: &str) -> Result<u64, ExitCode> {
    let (number, unit) = match text.as_bytes().last() {
        Some(b'k' | b'K') => (&text[..text.len() - 1], 1u64 << 10),
        Some(b'm' | b'M') => (&text[..text.len() - 1], 1 << 20),
        Some(b'g' | b'G') => (&text[..text.len() - 1], 1 << 30),
        _ => (text, 1),
    };
    number
        .parse::<u64>()
        .ok()
        .and_then(|count| count.checked_mul(unit))
        .ok_or_else(usage)
}

fn parse(argv: &[String]) -> Result<Args, ExitCode> {
    let mut args = Args {
        threads: available_threads(),
        max_memory: None,
        files: Vec::new(),
        is_any_input_allowed: false,
        output: None,
        should_overwrite: false,
        should_remove_inputs: false,
    };
    let mut index = 0;
    while let Some(arg) = argv.get(index) {
        let mut value = || {
            index += 1;
            argv.get(index).cloned().ok_or_else(usage)
        };
        match arg.as_str() {
            "-j" | "--threads" => {
                let threads: usize = value()?.parse().map_err(|_| usage())?;
                args.threads = if threads == 0 {
                    available_threads()
                } else {
                    threads
                };
            }
            "--max-memory" => args.max_memory = Some(size(&value()?)?),
            "--allow-non-demo" => args.is_any_input_allowed = true,
            "-o" | "--output" => args.output = Some(value()?),
            "-f" | "--force" => args.should_overwrite = true,
            "--rm" => args.should_remove_inputs = true,
            "-" => args.files.push(arg.clone()),
            _ if arg.starts_with('-') => return Err(usage()),
            _ => args.files.push(arg.clone()),
        }
        index += 1;
    }
    Ok(args)
}

/// Reads a whole input file (or standard input), once.
fn try_read_input(path: &str) -> Result<Vec<u8>, String> {
    if path == "-" {
        let mut buf = Vec::new();
        std::io::stdin()
            .lock()
            .read_to_end(&mut buf)
            .map_err(|error| error.to_string())?;
        Ok(buf)
    } else {
        std::fs::read(path).map_err(|error| error.to_string())
    }
}

/// Opens an input file (or standard input) to read in order.
fn open_input(path: &str) -> Result<Box<dyn Read>, String> {
    if path == "-" {
        Ok(Box::new(std::io::stdin().lock()))
    } else {
        let file = std::fs::File::open(path).map_err(|error| error.to_string())?;
        Ok(Box::new(std::io::BufReader::new(file)))
    }
}

/// Writes `data` as an output.
fn try_write_output(path: &str, data: &[u8]) -> Result<(), String> {
    try_write_with(path, |writer| {
        writer.write_all(data)?;
        writer.flush()
    })
}

/// Writes an output with `fill`: standard output, a device or pipe as it is, a
/// regular file directly (removed again when `fill` fails, as gzip and zstd do).
/// ponytail: a failed `-f` overwrite loses the old file and a killed process leaves
/// a partial one; a temporary file and rename would prevent both.
fn try_write_with(
    path: &str,
    fill: impl FnOnce(&mut dyn Write) -> std::io::Result<()>,
) -> Result<(), String> {
    if path == "-" {
        return match fill(&mut std::io::stdout().lock()) {
            // a closed pipe ends the output quietly: the reader wanted no more
            Err(error) if error.kind() == std::io::ErrorKind::BrokenPipe => Ok(()),
            result => result.map_err(|error| error.to_string()),
        };
    }
    let path = std::path::Path::new(path);
    let is_special = std::fs::metadata(path).is_ok_and(|metadata| !metadata.is_file());
    if is_special {
        let mut device = std::fs::OpenOptions::new()
            .write(true)
            .open(path)
            .map_err(|error| error.to_string())?;
        return fill(&mut device).map_err(|error| error.to_string());
    }
    let mut file = std::fs::File::create(path).map_err(|error| error.to_string())?;
    fill(&mut file).map_err(|error| {
        let _ = std::fs::remove_file(path);
        error.to_string()
    })
}

/// Whether `name` ends with the ASCII suffix `suffix`, ignoring case.
fn has_suffix(name: &str, suffix: &str) -> bool {
    name.len() >= suffix.len()
        && name.as_bytes()[name.len() - suffix.len()..].eq_ignore_ascii_case(suffix.as_bytes())
}

/// The output named after `input`: game.mvd -> game.mvdz; game.mvdz ->
/// game.mvd.
fn output_name(input: &str, should_compress: bool) -> Result<String, String> {
    if input == "-" {
        return Ok("-".into());
    }
    if should_compress {
        if has_suffix(input, ".mvdz") {
            return Err("already .mvdz".into());
        }
        let stem = if has_suffix(input, ".mvd") {
            &input[..input.len() - 4]
        } else {
            input
        };
        Ok(format!("{stem}.mvdz"))
    } else if has_suffix(input, ".mvdz") {
        Ok(format!("{}.mvd", &input[..input.len() - 5]))
    } else {
        Err("not named *.mvdz (-o names the output)".into())
    }
}

/// An input and its output name, or why it has none.
type Job = (String, Result<String, String>);

/// The jobs of a compress or decompress command line.
fn jobs(args: &Args, should_compress: bool) -> Result<Vec<Job>, ExitCode> {
    let files = &args.files;
    if let Some(output) = &args.output {
        let [input] = files.as_slice() else {
            return Err(usage());
        };
        return Ok(vec![(input.clone(), Ok(output.clone()))]);
    }
    if files.is_empty() {
        return Err(usage());
    }
    // the two-name form `in out`: the second names an output
    let is_output = |name: &str| name == "-" || has_suffix(name, ".mvdz") == should_compress;
    if let [input, output] = files.as_slice()
        && is_output(output)
    {
        return Ok(vec![(input.clone(), Ok(output.clone()))]);
    }
    if files.len() > 1 && files.iter().any(|file| file == "-") {
        return Err(usage());
    }
    let mut seen = std::collections::HashMap::new();
    Ok(files
        .iter()
        .map(|input| {
            let output = output_name(input, should_compress).and_then(|output| {
                match seen.insert(output.clone(), input.clone()) {
                    Some(previous) => Err(format!("{output} is also the output of {previous}")),
                    None => Ok(output),
                }
            });
            (input.clone(), output)
        })
        .collect())
}

/// Runs every job of a compress or decompress command line: several files on -j
/// threads, one file at a time each. Errors are reported per file.
fn run_jobs(args: &Args, should_compress: bool) -> Result<(), ExitCode> {
    let jobs = jobs(args, should_compress)?;
    let (workers, threads) = if jobs.len() > 1 {
        (args.threads, 1)
    } else {
        (1, args.threads)
    };
    // each worker takes the next job until none is left
    let next = AtomicUsize::new(0);
    let has_failed = AtomicBool::new(false);
    let work = || {
        while let Some((input, output)) = jobs.get(next.fetch_add(1, Ordering::Relaxed)) {
            if let Err(error) = run_job(args, input, output, should_compress, threads) {
                eprintln!("error: {input}: {error}");
                has_failed.store(true, Ordering::Relaxed);
            }
        }
    };
    std::thread::scope(|scope| {
        for _ in 1..workers {
            scope.spawn(work);
        }
        work();
    });
    if has_failed.load(Ordering::Relaxed) {
        return Err(ExitCode::FAILURE);
    }
    Ok(())
}

/// Compresses or decompresses `input` into `output` (or the reason it has none).
fn run_job(
    args: &Args,
    input: &str,
    output: &Result<String, String>,
    should_compress: bool,
    threads: usize,
) -> Result<(), String> {
    let output = output.as_ref().map_err(Clone::clone)?;
    let is_existing_file =
        output != "-" && std::fs::metadata(output).is_ok_and(|metadata| metadata.is_file());
    if is_existing_file && !args.should_overwrite {
        return Err(format!("{output} exists (-f overwrites)"));
    }
    if should_compress {
        compress_one(args, input, output, threads)
    } else {
        decompress_one(args, input, output, threads)
    }?;
    if args.should_remove_inputs && input != "-" && output != "-" {
        std::fs::remove_file(input).map_err(|error| format!("--rm: {error}"))?;
    }
    Ok(())
}

/// One per logical core.
fn available_threads() -> usize {
    std::thread::available_parallelism().map_or(1, std::num::NonZero::get)
}

fn compress_one(args: &Args, input: &str, output: &str, threads: usize) -> Result<(), String> {
    // non-demos are refused by the library
    let src = try_read_input(input)?;
    let config = Config { threads };
    let result = if args.is_any_input_allowed {
        // store whatever it is (it still round-trips)
        mvdz::compress_any(&src, &config)
    } else {
        mvdz::compress_info(&src, &config)
    };
    let (file, _) = result.map_err(|error| match error {
        mvdz::Error::NotMvd(reason) if !reason.starts_with("gzip") => {
            format!("{error} (--allow-non-demo compresses it anyway, mostly stored)")
        }
        _ => error.to_string(),
    })?;
    try_write_output(output, &file).map_err(|error| format!("{output}: {error}"))
}

fn decompress_one(args: &Args, input: &str, output: &str, threads: usize) -> Result<(), String> {
    // decoded and written segment by segment: neither the file nor the
    // demo is held whole
    let reader = open_input(input)?;
    let limits = read_limits(args);
    try_write_with(output, |writer| {
        mvdz::decompress_stream(reader, writer, threads, &limits)?;
        Ok(())
    })
}

fn read_limits(args: &Args) -> Limits {
    let mut limits = Limits::default();
    if let Some(memory) = args.max_memory {
        limits.memory = memory;
    }
    limits
}

fn info(args: &Args) -> Result<(), ExitCode> {
    let fail = |message: String| {
        eprintln!("error: {message}");
        ExitCode::FAILURE
    };
    let mut stdout = std::io::stdout().lock();
    for path in &args.files {
        let file = std::fs::File::open(path).map_err(|error| fail(format!("{path}: {error}")))?;
        let file_info = mvdz::read_info(std::io::BufReader::new(file))
            .map_err(|error| fail(format!("{path}: {error}")))?;
        match writeln!(stdout, "{}", info_json(&file_info)) {
            // a closed pipe ends the output quietly: the reader wanted no more
            Err(error) if error.kind() == std::io::ErrorKind::BrokenPipe => return Ok(()),
            result => result.map_err(|error| fail(error.to_string()))?,
        }
    }
    Ok(())
}

/// One object: `version`, `file_size`, `size`, `duration_ms`, `crc32` and `sha256`
/// (hexadecimal) and `segments`, a list of `{t_start_ms, raw_offset, raw_len}`.
fn info_json(file_info: &mvdz::FileInfo) -> String {
    use std::fmt::Write as _;
    let header = &file_info.header;
    // numbers and hexadecimal strings only: nothing needs escaping
    let mut json = format!(
        "{{\"version\":{},\"file_size\":{},\"size\":{},\"duration_ms\":{},\"crc32\":\"{:08x}\",\"sha256\":\"",
        mvdz::VERSION,
        file_info.file_size,
        header.size,
        header.duration_ms,
        header.crc,
    );
    for byte in header.sha256 {
        let _ = write!(json, "{byte:02x}");
    }
    json.push_str("\",\"segments\":[");
    for (index, segment) in file_info.index.segments.iter().enumerate() {
        let separator = if index > 0 { "," } else { "" };
        let _ = write!(
            json,
            "{separator}{{\"t_start_ms\":{},\"raw_offset\":{},\"raw_len\":{}}}",
            segment.start_ms, segment.raw_offset, segment.raw_len
        );
    }
    json.push_str("]}");
    json
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Context as _;
    use pretty_assertions::assert_eq;

    #[test]
    fn failed_output_is_removed() -> anyhow::Result<()> {
        let dir = std::env::temp_dir().join(format!("mvdz-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir)?;
        let path = dir.join("out");
        let name = path.to_str().context("a UTF-8 path")?;
        try_write_with(name, |writer| writer.write_all(b"one")).map_err(anyhow::Error::msg)?;
        assert_eq!(std::fs::read(&path)?, b"one");
        let failed = try_write_with(name, |_| Err(std::io::Error::other("fill")));
        assert!(failed.is_err());
        assert!(!path.exists());
        std::fs::remove_dir_all(&dir)?;
        Ok(())
    }

    #[test]
    fn info_json_describes_the_file() -> anyhow::Result<()> {
        use std::fmt::Write as _;
        // several segments of 256 KiB
        let demo = vec![7u8; 1 << 20];
        let (file, _) = mvdz::compress_any(&demo, &mvdz::Config::default())?;
        let file_info = mvdz::read_info(std::io::Cursor::new(&file))?;
        let json: serde_json::Value = serde_json::from_str(&info_json(&file_info))?;
        let header = &file_info.header;
        let sha256 = header.sha256.iter().fold(String::new(), |mut hex, byte| {
            let _ = write!(hex, "{byte:02x}");
            hex
        });
        assert_eq!(json["version"], mvdz::VERSION);
        assert_eq!(json["file_size"], file.len());
        assert_eq!(json["size"], demo.len());
        assert_eq!(json["duration_ms"], header.duration_ms);
        assert_eq!(json["crc32"], format!("{:08x}", header.crc));
        assert_eq!(json["sha256"], sha256);
        let segments = json["segments"].as_array().context("a segment list")?;
        assert_eq!(segments.len(), file_info.index.segments.len());
        for (segment, entry) in file_info.index.segments.iter().zip(segments) {
            assert_eq!(entry["t_start_ms"], segment.start_ms);
            assert_eq!(entry["raw_offset"], segment.raw_offset);
            assert_eq!(entry["raw_len"], segment.raw_len);
        }
        Ok(())
    }
}
