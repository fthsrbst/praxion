//! `praxion-forwarder`: bind the game and tee listeners, then run until stopped.

use std::process::ExitCode;

use praxion_forwarder::{Config, Forwarder};

fn main() -> ExitCode {
    let config = match Config::parse(std::env::args().skip(1)) {
        Ok(config) => config,
        Err(error) => {
            eprintln!("praxion-forwarder: {error}");
            eprintln!(
                "usage: praxion-forwarder [--listen 127.0.0.1:25565] [--upstream 127.0.0.1:25566] \
                 [--tee-listen 127.0.0.1:25600] [--stats <file.jsonl>] \
                 [--tee-queue-bytes 67108864]"
            );
            return ExitCode::from(2);
        }
    };
    let forwarder = match Forwarder::bind(config) {
        Ok(forwarder) => forwarder,
        Err(error) => {
            eprintln!("praxion-forwarder: cannot listen: {error}");
            return ExitCode::FAILURE;
        }
    };
    forwarder.run();
    ExitCode::SUCCESS
}
