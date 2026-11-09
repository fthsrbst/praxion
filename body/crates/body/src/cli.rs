//! `praxion-body`'s command line.
//!
//! ```text
//! praxion-body --forwarder 127.0.0.1:PORT --tee 127.0.0.1:PORT --name Praxion
//!              --goal get_wooden_pickaxe [--goal-args 3] --run-dir runs/<id>
//!              [--max-ticks N] [--settings body/config/reflexes.toml]
//!              [--skills body/skills] [--candidates DIR]
//!              [--start-delay-ticks N] [--style-seed N] [--linger-ticks N]
//!              [--verbose]
//!
//! praxion-body --forwarder ... --tee ... --run-dir runs/<id> --port 127.0.0.1:P
//!
//! praxion-body check-skills --candidates DIR [--skills body/skills]
//!
//! praxion-body check-blueprint <file> [--inventory '{"block": n}']
//!              [--max-placements N] [--emit-plan] [--anchor X Y Z]
//! ```
//!
//! `--forwarder` is the forwarder's game listener (the address a client would
//! connect to), `--tee` its tee listener. `--candidates` adds learned
//! `.skill` files to the core skills; `check-skills` checks them without a
//! server or network and prints one JSON report. In a port session the
//! directory is watched: a new or changed file reloads the program before the
//! next command is accepted, so a skill admitted after the body started is
//! playable without a restart.
//!
//! `--goal` is required unless `--port` is set: a port session takes its goals
//! from the brain, and `--goal` (when given) is injected as command `cli` so
//! there is one goal-start path (proposal §2.6).

use std::collections::BTreeMap;
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
    /// The hand-written goal. `None` is allowed only with `--port`.
    pub goal: Option<String>,
    pub goal_args: Vec<String>,
    /// The body port the brain connects to. `None` runs one goal and exits.
    pub port: Option<SocketAddr>,
    pub run_dir: PathBuf,
    pub max_ticks: Option<u64>,
    pub settings: Option<PathBuf>,
    pub skills: Option<PathBuf>,
    /// Learned candidate skills loaded next to the core skills; in a port
    /// session the directory is also watched for reloads.
    pub candidates: Option<PathBuf>,
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
        let mut port = None;
        let mut run_dir = None;
        let mut max_ticks = None;
        let mut settings = None;
        let mut skills = None;
        let mut candidates = None;
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
                        goal_args.extend(
                            next.split(',')
                                .filter(|part| !part.is_empty())
                                .map(str::to_owned),
                        );
                    }
                }
                "--run-dir" => run_dir = Some(PathBuf::from(value(&mut args, "--run-dir")?)),
                "--port" => port = Some(address(&mut args, "--port")?),
                "--max-ticks" => {
                    let text = value(&mut args, "--max-ticks")?;
                    max_ticks = Some(
                        text.parse()
                            .map_err(|_| format!("--max-ticks wants a number, got {text:?}"))?,
                    );
                }
                "--settings" => settings = Some(PathBuf::from(value(&mut args, "--settings")?)),
                "--skills" => skills = Some(PathBuf::from(value(&mut args, "--skills")?)),
                "--candidates" => {
                    candidates = Some(PathBuf::from(value(&mut args, "--candidates")?));
                }
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

        let forwarder = forwarder.ok_or("--forwarder is required")?;
        let tee = tee.ok_or("--tee is required")?;
        if goal.is_none() && port.is_none() {
            return Err("--goal is required (or --port for a brain session)".to_owned());
        }
        if goal.is_none() && !goal_args.is_empty() {
            return Err("--goal-args needs --goal".to_owned());
        }
        let run_dir = run_dir.ok_or("--run-dir is required")?;

        Ok(Self {
            forwarder,
            tee,
            name,
            goal,
            goal_args,
            port,
            run_dir,
            max_ticks,
            settings,
            skills,
            candidates,
            start_delay_ticks,
            style_seed,
            linger_ticks,
            verbose,
        })
    }
}

/// Parsed arguments of the `check-skills` subcommand.
#[derive(Clone, Debug, PartialEq)]
pub struct CheckArgs {
    pub candidates: PathBuf,
    pub skills: Option<PathBuf>,
}

impl CheckArgs {
    pub fn parse<I: IntoIterator<Item = String>>(args: I) -> Result<Self, String> {
        let mut candidates = None;
        let mut skills = None;

        let mut args = args.into_iter();
        while let Some(flag) = args.next() {
            match flag.as_str() {
                "--candidates" => {
                    let text = args.next().ok_or("--candidates wants a value")?;
                    candidates = Some(PathBuf::from(text));
                }
                "--skills" => {
                    let text = args.next().ok_or("--skills wants a value")?;
                    skills = Some(PathBuf::from(text));
                }
                other => return Err(format!("unknown argument {other:?}")),
            }
        }

        Ok(Self {
            candidates: candidates.ok_or("--candidates is required")?,
            skills,
        })
    }
}

/// Parsed arguments of the `check-blueprint` subcommand.
#[derive(Clone, Debug, PartialEq)]
pub struct BlueprintCheckArgs {
    pub file: PathBuf,
    /// Short block id to count, for the material rule. `None` skips the rule
    /// and only reports the plan's materials.
    pub inventory: Option<BTreeMap<String, u32>>,
    /// Overrides the slice-0 placement budget; `None` keeps
    /// [`praxion_blueprint::Limits::SLICE0`].
    pub max_placements: Option<usize>,
    /// Print the resolved plan (blueprint cells, world positions, block ids)
    /// so the referee never re-parses the format.
    pub emit_plan: bool,
    /// World position of blueprint cell `(0, 0, 0)`.
    pub anchor: (i32, i32, i32),
}

impl BlueprintCheckArgs {
    pub fn parse<I: IntoIterator<Item = String>>(args: I) -> Result<Self, String> {
        let mut file = None;
        let mut inventory = None;
        let mut max_placements = None;
        let mut emit_plan = false;
        let mut anchor = (0, 0, 0);

        let mut args = args.into_iter();
        while let Some(flag) = args.next() {
            match flag.as_str() {
                "--inventory" => {
                    let text = args.next().ok_or("--inventory wants a JSON object")?;
                    let raw: BTreeMap<String, u32> = serde_json::from_str(&text).map_err(
                        |error| format!("--inventory wants {{\"block\": count}}, got {error}"),
                    )?;
                    inventory = Some(
                        raw.into_iter()
                            .map(|(block, count)| {
                                (
                                    block
                                        .strip_prefix("minecraft:")
                                        .unwrap_or(block.as_str())
                                        .to_owned(),
                                    count,
                                )
                            })
                            .collect(),
                    );
                }
                "--max-placements" => {
                    let text = args.next().ok_or("--max-placements wants a number")?;
                    max_placements = Some(
                        text.parse()
                            .map_err(|_| format!("--max-placements wants a number, got {text:?}"))?,
                    );
                }
                "--emit-plan" => emit_plan = true,
                "--anchor" => {
                    let mut numbers = [0i32; 3];
                    for slot in &mut numbers {
                        let text = args.next().ok_or("--anchor wants three numbers")?;
                        *slot = text
                            .parse()
                            .map_err(|_| format!("--anchor wants three numbers, got {text:?}"))?;
                    }
                    anchor = (numbers[0], numbers[1], numbers[2]);
                }
                other if other.starts_with("--") => {
                    return Err(format!("unknown argument {other:?}"));
                }
                other => {
                    if file.is_some() {
                        return Err(format!("only one blueprint file, got {other:?} too"));
                    }
                    file = Some(PathBuf::from(other));
                }
            }
        }

        Ok(Self {
            file: file.ok_or("a blueprint file is required")?,
            inventory,
            max_placements,
            emit_plan,
            anchor,
        })
    }
}

/// Type a goal's arguments from the skill's declared parameters.
pub fn resolve_goal_args(
    program: &Program,
    skill: &str,
    raw: &[String],
) -> Result<Vec<ArgValue>, String> {
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
                    other => {
                        return Err(format!(
                            "`{}` wants true or false, got {other:?}",
                            param.name
                        ));
                    }
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
        assert_eq!(args.goal.as_deref(), Some("get_wood(2)"));
        assert_eq!(args.port, None);
        assert_eq!(args.run_dir, PathBuf::from("runs/x"));
        assert_eq!(args.max_ticks, Some(1200));
        assert_eq!(
            args.settings,
            Some(PathBuf::from("body/config/reflexes.toml"))
        );
        assert_eq!(args.style_seed, 7);
        assert_eq!(args.linger_ticks, 60);
        assert_eq!(args.candidates, None);
    }

    #[test]
    fn candidates_are_optional_on_the_play_command_line() {
        let args = parse(&[
            "--forwarder",
            "127.0.0.1:1",
            "--tee",
            "127.0.0.1:2",
            "--goal",
            "g",
            "--run-dir",
            "r",
            "--candidates",
            "candidates/",
        ])
        .unwrap();
        assert_eq!(args.candidates, Some(PathBuf::from("candidates/")));
    }

    #[test]
    fn check_skills_arguments_parse() {
        let args = CheckArgs::parse(
            ["--candidates", "candidates/", "--skills", "body/skills"]
                .into_iter()
                .map(str::to_owned),
        )
        .unwrap();
        assert_eq!(args.candidates, PathBuf::from("candidates/"));
        assert_eq!(args.skills, Some(PathBuf::from("body/skills")));

        let defaults = CheckArgs::parse(["--candidates", "c"].map(str::to_owned)).unwrap();
        assert_eq!(defaults.skills, None);
        assert_eq!(
            CheckArgs::parse(std::iter::empty()).unwrap_err(),
            "--candidates is required"
        );
    }

    #[test]
    fn check_blueprint_arguments_parse() {
        let args = BlueprintCheckArgs::parse(
            [
                "local/blueprints/shelter_1.bp",
                "--inventory",
                "{\"minecraft:cobblestone\": 74, \"torch\": 4}",
                "--max-placements",
                "150",
                "--emit-plan",
                "--anchor",
                "112",
                "64",
                "-38",
            ]
            .map(str::to_owned),
        )
        .unwrap();
        assert_eq!(args.file, PathBuf::from("local/blueprints/shelter_1.bp"));
        assert_eq!(
            args.inventory,
            Some(
                [
                    ("cobblestone".to_owned(), 74),
                    ("torch".to_owned(), 4)
                ]
                .into_iter()
                .collect()
            )
        );
        assert_eq!(args.max_placements, Some(150));
        assert!(args.emit_plan);
        assert_eq!(args.anchor, (112, 64, -38));

        let minimal = BlueprintCheckArgs::parse(["shelter.bp"].map(str::to_owned)).unwrap();
        assert_eq!(minimal.inventory, None);
        assert_eq!(minimal.max_placements, None);
        assert!(!minimal.emit_plan);
        assert_eq!(minimal.anchor, (0, 0, 0));
    }

    #[test]
    fn check_blueprint_argument_errors_are_named() {
        assert_eq!(
            BlueprintCheckArgs::parse(std::iter::empty()).unwrap_err(),
            "a blueprint file is required"
        );
        assert_eq!(
            BlueprintCheckArgs::parse(["--inventory"].map(str::to_owned)).unwrap_err(),
            "--inventory wants a JSON object"
        );
        assert!(
            BlueprintCheckArgs::parse(["--inventory", "[1]"].map(str::to_owned))
                .unwrap_err()
                .contains("--inventory wants")
        );
        assert_eq!(
            BlueprintCheckArgs::parse(["--anchor", "1", "2"].map(str::to_owned)).unwrap_err(),
            "--anchor wants three numbers"
        );
        assert_eq!(
            BlueprintCheckArgs::parse(["--wat"].map(str::to_owned)).unwrap_err(),
            "unknown argument \"--wat\""
        );
        assert_eq!(
            BlueprintCheckArgs::parse(["a.bp", "b.bp"].map(str::to_owned)).unwrap_err(),
            "only one blueprint file, got \"b.bp\" too"
        );
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
            "--goal is required (or --port for a brain session)"
        );
    }

    #[test]
    fn a_port_session_needs_no_goal() {
        let args = parse(&[
            "--forwarder",
            "127.0.0.1:1",
            "--tee",
            "127.0.0.1:2",
            "--run-dir",
            "r",
            "--port",
            "127.0.0.1:25650",
        ])
        .unwrap();
        assert_eq!(args.goal, None);
        assert_eq!(args.port.map(|addr| addr.port()), Some(25650));

        // A goal may still be given; it becomes command `cli`.
        let both = parse(&[
            "--forwarder",
            "127.0.0.1:1",
            "--tee",
            "127.0.0.1:2",
            "--run-dir",
            "r",
            "--port",
            "127.0.0.1:2",
            "--goal",
            "g",
        ])
        .unwrap();
        assert_eq!(both.goal.as_deref(), Some("g"));

        assert_eq!(
            parse(&[
                "--forwarder",
                "127.0.0.1:1",
                "--tee",
                "127.0.0.1:2",
                "--run-dir",
                "r",
                "--port",
                "127.0.0.1:2",
                "--goal-args",
                "3",
            ])
            .unwrap_err(),
            "--goal-args needs --goal"
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
