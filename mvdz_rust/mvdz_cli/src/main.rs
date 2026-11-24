use std::io::{Read, Write};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(code) => code,
    }
}

fn run() -> Result<(), ExitCode> {
    let command_line: Vec<String> = std::env::args().skip(1).collect();
    let Some((command, rest)) = command_line.split_first() else {
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

fn usage() -> ExitCode {
    eprintln!(
        "usage:
  mvdz compress   [-j threads] [-M mebibytes]
                  [-f] [--rm] files... | in out.mvdz | -o out.mvdz in
                  game.mvd -> game.mvdz beside it
  mvdz decompress [-j threads] [-M mebibytes]
                  [-f] [--rm] files... | in.mvdz out | -o out in.mvdz
                  game.mvdz -> game.mvd beside it
  mvdz info       file.mvdz...                     header as JSON
'-' as a file name is standard input or output. compress and decompress name
each output after its input, keep the inputs (--rm deletes each one after its
output is written), refuse to overwrite an existing file (-f overwrites), and
with several files work on -j of them at once; two names where the second is
an output (compress: *.mvdz or -; decompress: not *.mvdz) are input and
output, as is -o. compress takes a raw demo: input that is not an MVD demo
is refused, and a .mvd.gz is to be decompressed first
(gzip -dc game.mvd.gz | mvdz compress - game.mvdz). Compression has no
settings: the demo is cut into independently decodable segments of 256 KiB.
Both work on up to -j segments at once. -j is 0 to 255, at most the logical
cores; 0 (the default) is all of them. -M is the memory to stay within for
each file, approximately: fewer segments are worked on at once, never fewer
than one. Without -M, decompress stays within 64 MiB and compress has no
limit. The memory does not grow with the demo, except for compress from
standard input, which holds the demo."
    );
    ExitCode::from(2)
}

#[derive(Debug)]
struct Args {
    max_threads: u8,
    max_memory_mebibytes: Option<u64>,
    files: Vec<String>,
    output: Option<String>,
    should_overwrite: bool,
    should_remove_inputs: bool,
}

fn parse(arguments: &[String]) -> Result<Args, ExitCode> {
    let mut args = Args {
        max_threads: 0,
        max_memory_mebibytes: None,
        files: Vec::new(),
        output: None,
        should_overwrite: false,
        should_remove_inputs: false,
    };
    let mut index = 0;
    while let Some(arg) = arguments.get(index) {
        let mut value = || {
            index += 1;
            arguments.get(index).cloned().ok_or_else(usage)
        };
        match arg.as_str() {
            "-j" | "--threads" => {
                args.max_threads = value()?.parse().map_err(|_| usage())?;
            }
            "-M" | "--memory" => {
                args.max_memory_mebibytes = Some(value()?.parse().map_err(|_| usage())?);
            }
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

fn open_input(path: &str) -> Result<Box<dyn Read>, String> {
    if path == "-" {
        Ok(Box::new(std::io::stdin().lock()))
    } else {
        let file = std::fs::File::open(path).map_err(|error| error.to_string())?;
        Ok(Box::new(std::io::BufReader::new(file)))
    }
}

/// A regular file is removed again when `write_output` fails, as gzip and zstd do.
/// ponytail: a failed `-f` overwrite loses the old file and a killed process leaves
/// a partial one; a temporary file and rename would prevent both.
fn try_write_with(
    path: &str,
    write_output: impl FnOnce(&mut dyn Write) -> std::io::Result<()>,
) -> Result<(), String> {
    if path == "-" {
        return match write_output(&mut std::io::stdout().lock()) {
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
        return write_output(&mut device).map_err(|error| error.to_string());
    }
    let mut file = std::fs::File::create(path).map_err(|error| error.to_string())?;
    write_output(&mut file).map_err(|error| {
        let _ = std::fs::remove_file(path);
        error.to_string()
    })
}

fn has_suffix_ignoring_case(name: &str, suffix: &str) -> bool {
    name.len() >= suffix.len()
        && name.as_bytes()[name.len() - suffix.len()..].eq_ignore_ascii_case(suffix.as_bytes())
}

fn output_name(input: &str, should_compress: bool) -> Result<String, String> {
    if input == "-" {
        return Ok("-".into());
    }
    if should_compress {
        if has_suffix_ignoring_case(input, ".mvdz") {
            return Err("already .mvdz".into());
        }
        let stem = if has_suffix_ignoring_case(input, ".mvd") {
            &input[..input.len() - 4]
        } else {
            input
        };
        Ok(format!("{stem}.mvdz"))
    } else if has_suffix_ignoring_case(input, ".mvdz") {
        Ok(format!("{}.mvd", &input[..input.len() - 5]))
    } else {
        Err("not named *.mvdz (-o names the output)".into())
    }
}

/// An input and its output name, or why it has none.
type Job = (String, Result<String, String>);

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
    let is_output =
        |name: &str| name == "-" || has_suffix_ignoring_case(name, ".mvdz") == should_compress;
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

fn run_jobs(args: &Args, should_compress: bool) -> Result<(), ExitCode> {
    let jobs = jobs(args, should_compress)?;
    let (workers, max_threads) = if jobs.len() > 1 {
        (thread_count(args.max_threads), 1)
    } else {
        (1, args.max_threads)
    };
    let next = AtomicUsize::new(0);
    let has_failed = AtomicBool::new(false);
    let work = || {
        while let Some((input, output)) = jobs.get(next.fetch_add(1, Ordering::Relaxed)) {
            if let Err(error) = run_job(args, input, output, should_compress, max_threads) {
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

fn run_job(
    args: &Args,
    input: &str,
    output: &Result<String, String>,
    should_compress: bool,
    max_threads: u8,
) -> Result<(), String> {
    let output = output.as_ref().map_err(Clone::clone)?;
    let is_existing_file =
        output != "-" && std::fs::metadata(output).is_ok_and(|metadata| metadata.is_file());
    if is_existing_file && !args.should_overwrite {
        return Err(format!("{output} exists (-f overwrites)"));
    }
    let max_memory = args.max_memory_mebibytes.map(|mebibytes| mebibytes << 20);
    if should_compress {
        compress_one(input, output, max_threads, max_memory)
    } else {
        decompress_one(input, output, max_threads, max_memory)
    }?;
    if args.should_remove_inputs && input != "-" && output != "-" {
        std::fs::remove_file(input).map_err(|error| format!("--rm: {error}"))?;
    }
    Ok(())
}

/// Counts as the library does.
fn thread_count(max_threads: u8) -> usize {
    let available = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
    match max_threads {
        0 => available,
        max_threads => usize::from(max_threads).min(available),
    }
}

fn compress_one(
    input: &str,
    output: &str,
    max_threads: u8,
    max_memory: Option<u64>,
) -> Result<(), String> {
    let mut encoder = mvdz::Encoder::new().max_threads(max_threads);
    if let Some(max_memory) = max_memory {
        encoder = encoder.max_memory(max_memory);
    }
    try_write_with(output, |writer| {
        if input == "-" {
            // read twice: a pipe is held whole
            let mut demo = Vec::new();
            std::io::stdin().lock().read_to_end(&mut demo)?;
            encoder.encode(std::io::Cursor::new(demo), writer)?;
        } else {
            encoder.encode(std::fs::File::open(input)?, writer)?;
        }
        Ok(())
    })
}

fn decompress_one(
    input: &str,
    output: &str,
    max_threads: u8,
    max_memory: Option<u64>,
) -> Result<(), String> {
    // segment by segment: neither the file nor the demo is held whole
    let reader = open_input(input)?;
    try_write_with(output, |writer| {
        let mut decoder = mvdz::Decoder::new(reader)?.max_threads(max_threads);
        if let Some(max_memory) = max_memory {
            decoder = decoder.max_memory(max_memory);
        }
        std::io::copy(&mut decoder, writer)?;
        writer.flush()
    })
}

fn info(args: &Args) -> Result<(), ExitCode> {
    let fail = |message: String| {
        eprintln!("error: {message}");
        ExitCode::FAILURE
    };
    let mut stdout = std::io::stdout().lock();
    for path in &args.files {
        let file = std::fs::File::open(path).map_err(|error| fail(format!("{path}: {error}")))?;
        let decoder = mvdz::Decoder::new(std::io::BufReader::new(file))
            .map_err(|error| fail(format!("{path}: {error}")))?;
        match writeln!(stdout, "{}", header_json(decoder.header())) {
            Err(error) if error.kind() == std::io::ErrorKind::BrokenPipe => return Ok(()),
            result => result.map_err(|error| fail(error.to_string()))?,
        }
    }
    Ok(())
}

fn header_json(header: &mvdz::Header) -> String {
    use std::fmt::Write as _;
    // numbers and hexadecimal strings only: nothing needs escaping
    let mut json = format!(
        "{{\"size_bytes\":{},\"duration_ms\":{},\"crc32\":\"{:08x}\",\"sha256\":\"",
        header.size(),
        header.duration().as_millis(),
        header.crc32(),
    );
    for byte in header.sha256() {
        let _ = write!(json, "{byte:02x}");
    }
    json.push_str("\"}");
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
    fn header_json_describes_the_demo() -> anyhow::Result<()> {
        use std::fmt::Write as _;
        const DEM_ALL: u8 = 6;
        const SVC_NOP: u8 = 1;
        let payload = [SVC_NOP; 1000];
        let frame = [&[1, DEM_ALL][..], &1000u32.to_le_bytes(), &payload].concat();
        let demo = frame.repeat(1100);
        let file = mvdz::encode(&demo, 0)?;
        let decoder = mvdz::Decoder::new(std::io::Cursor::new(&file))?;
        let header = decoder.header();
        let json: serde_json::Value = serde_json::from_str(&header_json(header))?;
        let sha256 = header.sha256().iter().fold(String::new(), |mut hex, byte| {
            let _ = write!(hex, "{byte:02x}");
            hex
        });
        assert_eq!(json["size_bytes"], demo.len());
        assert_eq!(json["duration_ms"], 1100);
        assert_eq!(json["crc32"], format!("{:08x}", header.crc32()));
        assert_eq!(json["sha256"], sha256);
        Ok(())
    }
}
