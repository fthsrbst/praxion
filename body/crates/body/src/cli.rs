//! `praxion-body`'s command line.
//!
//! ```text
//! praxion-body --forwarder 127.0.0.1:PORT --tee 127.0.0.1:PORT --name Praxion
//!              --goal get_wooden_pickaxe [--goal-args 3] --run-dir runs/<id>
//!              [--max-ticks N] [--settings body/config/reflexes.toml]
//!              [--skills body/skills] [--start-delay-ticks N]
//!              [--style-seed N] [--linger-ticks N] [--verbose]
//! ```
//!
//! `--forwarder` is the forwarder's game listener (the address a client would
//! connect to), `--tee` its tee listener.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::str::FromStr;

use praxion_rules::{ArgValue, Program};

/// Parsed arguments.
#[derive(Clone, Debug, PartialEq)]
pub struct Args {
    pub forwarder: SocketAddr,
    pub tee: SocketAddr,
    pub name: String,
    pub goal: String,
    pub goal_args: Vec<String>,
    pub run_dir: PathBuf,
    pub max_ticks: Option<u64>,
    pub settings: Option<PathBuf>,
    pub skills: Option<PathBuf>,
    pub start_delay_ticks: u64,
    /// The movement-style seed; the same seed gives the same personality.
    pub style_seed: u64,
    pub linger_ticks: u64,
    pub verbose: bool,
}

impl Args {
    pub fn parse<I: IntoIterator<Item = String>>(args: I) -> Result<Self, String> {
        let mut forwarder = None;
        let mut tee = None;
        let mut name = "Praxion".to_owned();
        let mut goal = None;
        let mut goal_args = Vec::new();
        let mut run_dir = None;
        let mut max_ticks = None;
        let mut settings = None;
        let mut skills = None;
        let mut start_delay_ticks = 0;
        let mut style_seed = 0;
        let mut linger_ticks = 60;
        let mut verbose = false;

        let mut args = args.into_iter().peekable();
        while let Some(flag) = args.next() {
            match flag.as_str() {
                "--forwarder" => forwarder = Some(address(&mut args, "--forwarder")?),
                "--tee" => tee = Some(address(&mut args, "--tee")?),
                "--name" => name = value(&mut args, "--name")?,
                "--goal" => goal = Some(value(&mut args, "--goal")?),
                "--goal-args" => {
                    // Every following token that is not a flag is a goal
                    // argument; commas separate several on one token.
                    while let Some(next) = args.peek()
                        && !next.starts_with("--")
                    {
                        let next = args.next().expect("peeked");
                        goal_args.extend(next.split(',').filter(|part| !part.is_empty()).map(str::to_owned));
                    }
                }
                "--run-dir" => run_dir = Some(PathBuf::from(value(&mut args, "--run-dir")?)),
                "--max-ticks" => {
                    let text = value(&mut args, "--max-ticks")?;
                    max_ticks = Some(
                        text.parse()
                            .map_err(|_| format!("--max-ticks wants a number, got {text:?}"))?,
                    );
                }
                "--settings" => settings = Some(PathBuf::from(value(&mut args, "--settings")?)),
                "--skills" => skills = Some(PathBuf::from(value(&mut args, "--skills")?)),
                "--start-delay-ticks" => {
                    let text = value(&mut args, "--start-delay-ticks")?;
                    start_delay_ticks = text
                        .parse()
                        .map_err(|_| format!("--start-delay-ticks wants a number, got {text:?}"))?;
                }
                "--style-seed" => {
                    let text = value(&mut args, "--style-seed")?;
                    style_seed = text
                        .parse()
                        .map_err(|_| format!("--style-seed wants a number, got {text:?}"))?;
                }
                "--linger-ticks" => {
                    let text = value(&mut args, "--linger-ticks")?;
                    linger_ticks = text
                        .parse()
                        .map_err(|_| format!("--linger-ticks wants a number, got {text:?}"))?;
                }
                "--verbose" => verbose = true,
                other => return Err(format!("unknown argument {other:?}")),
            }
        }

        Ok(Self {
            forwarder: forwarder.ok_or("--forwarder is required")?,
            tee: tee.ok_or("--tee is required")?,
            name,
            goal: goal.ok_or("--goal is required")?,
            goal_args,
            run_dir: run_dir.ok_or("--run-dir is required")?,
            max_ticks,
            settings,
            skills,
            start_delay_ticks,
            style_seed,
            linger_ticks,
            verbose,
        })
    }
}

/// Type a goal's arguments from the skill's declared parameters.
pub fn resolve_goal_args(program: &Program, skill: &str, raw: &[String]) -> Result<Vec<ArgValue>, String> {
    let declared = program
        .skill(skill)
        .ok_or_else(|| format!("the ruleset has no skill `{skill}`"))?;
    if raw.len() > declared.params.len() {
        return Err(format!(
            "skill `{skill}` takes at most {} arguments, got {}",
            declared.params.len(),
            raw.len()
        ));
    }
    raw.iter()
        .zip(&declared.params)
        .map(|(text, param)| {
            let argument = match param.ty {
                praxion_rules::ValueType::Int => text
                    .parse()
                    .map(ArgValue::Int)
                    .map_err(|_| format!("`{}` wants an int, got {text:?}", param.name))?,
                praxion_rules::ValueType::Item => ArgValue::Item(text.clone()),
                praxion_rules::ValueType::Block => ArgValue::Block(text.clone()),
                praxion_rules::ValueType::EntityKind => ArgValue::EntityKind(text.clone()),
                praxion_rules::ValueType::Bool => match text.as_str() {
                    "true" => ArgValue::Bool(true),
                    "false" => ArgValue::Bool(false),
                    other => return Err(format!("`{}` wants true or false, got {other:?}", param.name)),
                },
            };
            Ok(argument)
        })
        .collect()
}

fn value<I: Iterator<Item = String>>(args: &mut I, flag: &str) -> Result<String, String> {
    args.next().ok_or_else(|| format!("{flag} wants a value"))
}

fn address<I: Iterator<Item = String>>(args: &mut I, flag: &str) -> Result<SocketAddr, String> {
    let text = value(args, flag)?;
    SocketAddr::from_str(&text)
        .map_err(|error| format!("{flag} wants 127.0.0.1:PORT, got {text:?}: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use praxion_rules::load;

    fn parse(parts: &[&str]) -> Result<Args, String> {
        Args::parse(parts.iter().map(|part| (*part).to_owned()))
    }

    #[test]
    fn the_full_command_line_parses() {
        let args = parse(&[
            "--forwarder",
            "127.0.0.1:25640",
            "--tee",
            "127.0.0.1:25641",
            "--name",
            "Praxy",
            "--goal",
            "get_wood(2)",
            "--run-dir",
            "runs/x",
            "--max-ticks",
            "1200",
            "--settings",
            "body/config/reflexes.toml",
            "--style-seed",
            "7",
        ])
        .unwrap();
        assert_eq!(args.forwarder.port(), 25640);
        assert_eq!(args.tee.port(), 25641);
        assert_eq!(args.name, "Praxy");
        assert_eq!(args.goal, "get_wood(2)");
        assert_eq!(args.run_dir, PathBuf::from("runs/x"));
        assert_eq!(args.max_ticks, Some(1200));
        assert_eq!(args.settings, Some(PathBuf::from("body/config/reflexes.toml")));
        assert_eq!(args.style_seed, 7);
        assert_eq!(args.linger_ticks, 60);
    }

    #[test]
    fn goal_arguments_take_several_tokens_and_commas() {
        let args = parse(&[
            "--forwarder",
            "127.0.0.1:1",
            "--tee",
            "127.0.0.1:2",
            "--goal",
            "s",
            "--goal-args",
            "3",
            "cobblestone,2",
            "--run-dir",
            "r",
        ])
        .unwrap();
        assert_eq!(args.goal_args, vec!["3", "cobblestone", "2"]);
    }

    #[test]
    fn a_missing_required_flag_is_named() {
        assert_eq!(parse(&[]).unwrap_err(), "--forwarder is required");
        assert_eq!(
            parse(&["--forwarder", "127.0.0.1:1", "--tee", "127.0.0.1:2"]).unwrap_err(),
            "--goal is required"
        );
    }

    #[test]
    fn goal_arguments_are_typed_from_the_skill() {
        let program = load(
            "skill g(n: int, what: item = stick)\n  rules:\n    count($what) >= $n -> done\n    true -> done\n",
        )
        .unwrap();
        assert_eq!(
            resolve_goal_args(&program, "g", &["3".to_owned(), "oak_log".to_owned()]).unwrap(),
            vec![ArgValue::Int(3), ArgValue::Item("oak_log".to_owned())]
        );
        assert!(resolve_goal_args(&program, "g", &["many".to_owned()]).is_err());
        assert!(resolve_goal_args(&program, "nope", &[]).is_err());
    }
}
