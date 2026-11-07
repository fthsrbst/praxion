//! `praxion-forwarder`: bind the game and tee listeners, then run until stopped.

use std::process::ExitCode;

use praxion_forwarder::{Config, Forwarder};

fn main() -> ExitCode {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let config = match Config::parse(std::env::args().skip(1)) {
        Ok(config) => config,
        Err(error) => {
            log::error!("praxion-forwarder: {error}");
            log::error!(
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
            log::error!("praxion-forwarder: cannot listen: {error}");
            return ExitCode::FAILURE;
        }
    };
    forwarder.run();
    ExitCode::SUCCESS
}
