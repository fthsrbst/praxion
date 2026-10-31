//! `decode <file.prxcap> [--json-out <path>] [--block X Y Z]... \
//!  [--dump <names|*> <out.jsonl>]`
//!
//! Exit code 0 when E2 passes (zero decode errors, zero unconsumed packets),
//! 1 when it fails, 2 on bad usage or an unreadable capture.

use std::io::{BufWriter, Write as _};
use std::process::ExitCode;

use azalea_core::position::BlockPos;
use decode::{DumpFilter, Options, analyze_bytes};

const USAGE: &str = "usage: decode <file.prxcap> [--json-out <path>] [--block X Y Z]... \
                     [--dump <names|*> <out.jsonl>]";

struct DumpSpec {
    filter: DumpFilter,
    path: String,
}

struct Args {
    capture: String,
    json_out: Option<String>,
    blocks: Vec<BlockPos>,
    dump: Option<DumpSpec>,
}

fn parse_args() -> Result<Args, String> {
    let mut capture = None;
    let mut json_out = None;
    let mut blocks = Vec::new();
    let mut dump = None;
    let mut args = std::env::args().skip(1);

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--json-out" => {
                let path = args.next().ok_or("--json-out needs a path")?;
                json_out = Some(path);
            }
            "--dump" => {
                if dump.is_some() {
                    return Err("--dump was given more than once".to_owned());
                }
                let names = args
                    .next()
                    .ok_or("--dump needs a comma-separated packet list")?;
                let path = args.next().ok_or("--dump needs an output path")?;
                dump = Some(DumpSpec {
                    filter: DumpFilter::parse(&names)?,
                    path,
                });
            }
            "--block" => {
                let mut numbers = [0i32; 3];
                for slot in numbers.iter_mut() {
                    let value = args
                        .next()
                        .ok_or("--block needs three integers (X Y Z)")?;
                    *slot = value
                        .parse()
                        .map_err(|_| format!("--block: '{value}' is not an i32"))?;
                }
                blocks.push(BlockPos::new(numbers[0], numbers[1], numbers[2]));
            }
            "-h" | "--help" => return Err(USAGE.to_owned()),
            other if other.starts_with('-') => {
                return Err(format!("unknown option '{other}'\n{USAGE}"));
            }
            other => {
                if capture.is_some() {
                    return Err(format!("more than one capture file given\n{USAGE}"));
                }
                capture = Some(other.to_owned());
            }
        }
    }

    Ok(Args {
        capture: capture.ok_or_else(|| USAGE.to_owned())?,
        json_out,
        blocks,
        dump,
    })
}

fn write_dump(path: &str, records: &[decode::analysis::PacketRecord]) -> std::io::Result<u64> {
    let file = std::fs::File::create(path)?;
    let mut writer = BufWriter::new(file);
    let lines = decode::dump::write_records(&mut writer, records)?;
    writer.flush()?;
    Ok(lines)
}

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(args) => args,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::from(2);
        }
    };

    let bytes = match std::fs::read(&args.capture) {
        Ok(bytes) => bytes,
        Err(e) => {
            eprintln!("cannot read {}: {e}", args.capture);
            return ExitCode::from(2);
        }
    };

    let options = Options {
        blocks: args.blocks.clone(),
        dump: args.dump.as_ref().map(|dump| dump.filter.clone()),
    };
    let analysis = match analyze_bytes(&args.capture, &bytes, &options) {
        Ok(analysis) => analysis,
        Err(e) => {
            eprintln!("{}: {e}", args.capture);
            return ExitCode::from(2);
        }
    };

    print!("{}", analysis.text_report());

    if let Some(path) = &args.json_out {
        let json = analysis.json().render();
        if let Err(e) = std::fs::write(path, format!("{json}\n")) {
            eprintln!("cannot write {path}: {e}");
            return ExitCode::from(2);
        }
    }

    if let Some(spec) = &args.dump {
        match write_dump(&spec.path, &analysis.packets) {
            Ok(lines) => eprintln!("dump: {lines} packets -> {}", spec.path),
            Err(e) => {
                eprintln!("cannot write {}: {e}", spec.path);
                return ExitCode::from(2);
            }
        }
    }

    if analysis.passed() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
