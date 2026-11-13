//! `praxion-body`'s command line.
//!
//! [`USAGE`] is what `--help` prints: both play modes, the subcommands and every
//! flag.
//!
//! `--candidates` adds learned `.skill` files to the core skills; `check-skills`
//! checks them without a server or network and prints one JSON report. In a port
//! session the directory is watched: a new or changed file reloads the program
//! before the next command is accepted, so a skill admitted after the body
//! started is playable without a restart.
//!
//! `export-gamedata` writes the recipes, smelting recipes and item tags of the
//! pinned jar as one JSON file for the brain (decision 2026-11-09 "Five next
//! areas"); without `--out` it writes `local/gamedata/26.3.json` under the
//! repository root.
//!
//! `--operator` names a player whose chat the body obeys (repeatable; the
//! harness passes the watcher's name). Their lines are read from
//! `ClientboundPlayerChat` by sender UUID; `dur`, `iptal`, `durum` and `devam`
//! are acted on by the body itself, any other line goes to the brain.
//!
//! `--goal` is required unless `--port` is set: a port session takes its goals
//! from the brain, and `--goal` (when given) is injected as command `cli` so
//! there is one goal-start path (proposal §2.6).

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::str::FromStr;

use praxion_rules::{ArgValue, Program};

/// `--help`: both play modes, the subcommands and every flag.
pub const USAGE: &str = concat!(
    "usage: praxion-body --forwarder 127.0.0.1:PORT --tee 127.0.0.1:PORT\n",
    "                    --name Praxion --goal SKILL [--goal-args ...]\n",
    "                    --run-dir runs/<id> [options]\n",
    "       praxion-body --real-client --tee 127.0.0.1:PORT --name NAME\n",
    "                    --run-dir runs/<id> [--hands PATH --client-pid PID\n",
    "                    --client-options FILE [--hands-activate]] [options]\n",
    "       praxion-body check-skills --candidates DIR [--skills body/skills]\n",
    "       praxion-body check-blueprint <file> [--inventory JSON]\n",
    "                    [--max-placements N] [--emit-plan] [--anchor X Y Z]\n",
    "       praxion-body export-gamedata [--out local/gamedata/26.3.json]\n",
    "\n",
    "flags:\n",
    "  --forwarder ADDR       the forwarder's game listener, the address a client uses;\n",
    "                         required unless --real-client is set\n",
    "  --tee ADDR             the forwarder's tee listener; always required\n",
    "  --name NAME            the player name the body logs in with; with --real-client,\n",
    "                         the operator's own player name\n",
    "  --real-client          shadow mode: attach to the tee only and run the decision\n",
    "                         loop once per observed client tick; without --hands every\n",
    "                         frame is dropped and no OS input is posted\n",
    "  --hands PATH           the praxion-hands helper; with --client-pid and\n",
    "                         --client-options the frames are applied to the operator's\n",
    "                         own client instead of dropped (--real-client only)\n",
    "  --client-pid PID       the Minecraft client process the helper posts to\n",
    "  --client-options FILE  the client's options.txt (mouseSensitivity and the\n",
    "                         default key bindings the hands press)\n",
    "  --hands-activate       ask macOS to bring the client forward before arming\n",
    "  --hands-look-latency N ticks between a posted look and the client's own\n",
    "                         rotation packet (default 2)\n",
    "  --goal SKILL           the goal to play; with --port the body takes goals from the\n",
    "                         brain and this one is injected as command `cli`\n",
    "  --goal-args ...        the goal's arguments\n",
    "  --run-dir DIR          where the event log and summary.json go (required)\n",
    "  --port ADDR            listen for the brain on this address\n",
    "  --max-ticks N          stop after N client ticks\n",
    "  --settings FILE        the reflex settings (body/config/reflexes.toml)\n",
    "  --skills DIR           the core skills (body/skills)\n",
    "  --candidates DIR       extra learned skills, watched for reloads in a port session\n",
    "  --start-delay-ticks N  ticks to wait before the first goal\n",
    "  --style-seed N         the movement-style seed; the same seed is the same personality\n",
    "  --linger-ticks N       ticks to linger after a settled goal\n",
    "  --operator NAME        a player whose chat the body obeys (repeatable)\n",
    "  --chat-prefix TEXT     the prefix an operator's task line needs (`p:` by default)\n",
    "  --verbose              print the packet traces\n",
    "  -h, --help             this text\n",
);

/// Parsed arguments.
#[derive(Clone, Debug, PartialEq)]
pub struct Args {
    /// The forwarder's game listener. `None` only in `--real-client` mode, which
    /// never logs in.
    pub forwarder: Option<SocketAddr>,
    pub tee: SocketAddr,
    /// Shadow mode: run the decision loop on the operator's own client through
    /// the tee, without logging in and without acting.
    pub real_client: bool,
    /// The `praxion-hands` helper; the three hands paths are all or nothing.
    pub hands: Option<PathBuf>,
    /// The client process the helper posts to.
    pub client_pid: Option<u32>,
    /// The client's `options.txt`.
    pub client_options: Option<PathBuf>,
    /// Ask macOS to bring the client forward before arming.
    pub hands_activate: bool,
    /// Ticks between a posted look and the client's rotation packet.
    pub hands_look_latency: usize,
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
    /// Player names whose chat the body obeys (`--operator`, repeatable). With
    /// none the body reads no chat at all (architecture §7).
    pub operators: Vec<String>,
    /// The prefix an operator's task line needs in chat (`--chat-prefix`,
    /// `p:` by default); an empty value accepts every line as a task
    /// (decision 2026-11-09 "Chat: control words bare, tasks with a prefix").
    pub chat_prefix: String,
}

impl Args {
    pub fn parse<I: IntoIterator<Item = String>>(args: I) -> Result<Self, String> {
        let mut forwarder = None;
        let mut tee = None;
        let mut real_client = false;
        let mut hands = None;
        let mut client_pid = None;
        let mut client_options = None;
        let mut hands_activate = false;
        let mut hands_look_latency: usize = 2;
        let mut hands_look_latency_given = false;
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
        let mut operators = Vec::new();
        let mut chat_prefix = crate::chat::DEFAULT_TASK_PREFIX.to_owned();

        let mut args = args.into_iter().peekable();
        while let Some(flag) = args.next() {
            match flag.as_str() {
                "--forwarder" => forwarder = Some(address(&mut args, "--forwarder")?),
                "--tee" => tee = Some(address(&mut args, "--tee")?),
                "--real-client" => real_client = true,
                "--hands" => hands = Some(PathBuf::from(value(&mut args, "--hands")?)),
                "--client-pid" => {
                    let text = value(&mut args, "--client-pid")?;
                    client_pid = Some(
                        text.parse()
                            .map_err(|_| format!("--client-pid wants a process id, got {text:?}"))?,
                    );
                }
                "--client-options" => {
                    client_options = Some(PathBuf::from(value(&mut args, "--client-options")?));
                }
                "--hands-activate" => hands_activate = true,
                "--hands-look-latency" => {
                    let text = value(&mut args, "--hands-look-latency")?;
                    hands_look_latency = text.parse().map_err(|_| {
                        format!("--hands-look-latency wants a number, got {text:?}")
                    })?;
                    hands_look_latency_given = true;
                }
                "--name" => name = value(&mut args, "--name")?,
                "--operator" => operators.push(value(&mut args, "--operator")?),
                "--chat-prefix" => chat_prefix = value(&mut args, "--chat-prefix")?,
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

        let forwarder = match (forwarder, real_client) {
            (Some(forwarder), _) => Some(forwarder),
            // Shadow mode attaches to the tee only: it never logs in, so there is
            // no game listener to name.
            (None, true) => None,
            (None, false) => return Err("--forwarder is required".to_owned()),
        };
        let tee = tee.ok_or("--tee is required")?;
        if goal.is_none() && port.is_none() {
            return Err("--goal is required (or --port for a brain session)".to_owned());
        }
        if goal.is_none() && !goal_args.is_empty() {
            return Err("--goal-args needs --goal".to_owned());
        }
        let run_dir = run_dir.ok_or("--run-dir is required")?;

        // The hands are one mode: a helper, a client and its options, all
        // three or none. They only make sense on the operator's own client.
        let hands_requested = hands.is_some()
            || client_pid.is_some()
            || client_options.is_some()
            || hands_activate
            || hands_look_latency_given;
        if hands_requested && !real_client {
            return Err(
                "--hands, --client-pid, --client-options, --hands-activate and \
                 --hands-look-latency need --real-client"
                    .to_owned(),
            );
        }
        let hands_any = hands.is_some() || client_pid.is_some() || client_options.is_some();
        if hands_any && !(hands.is_some() && client_pid.is_some() && client_options.is_some()) {
            return Err(
                "--hands, --client-pid and --client-options are required together".to_owned(),
            );
        }
        if (hands_activate || hands_look_latency_given) && hands.is_none() {
            return Err(
                "--hands-activate and --hands-look-latency need --hands, --client-pid and \
                 --client-options"
                    .to_owned(),
            );
        }

        Ok(Self {
            forwarder,
            tee,
            real_client,
            hands,
            client_pid,
            client_options,
            hands_activate,
            hands_look_latency,
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
            operators,
            chat_prefix,
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

/// Parsed arguments of the `export-gamedata` subcommand.
#[derive(Clone, Debug, PartialEq)]
pub struct ExportArgs {
    /// Where to write the JSON; `None` is `local/gamedata/26.3.json` under the
    /// repository root.
    pub out: Option<PathBuf>,
}

impl ExportArgs {
    pub fn parse<I: IntoIterator<Item = String>>(args: I) -> Result<Self, String> {
        let mut out = None;
        let mut args = args.into_iter();
        while let Some(flag) = args.next() {
            match flag.as_str() {
                "--out" => {
                    let text = args.next().ok_or("--out wants a value")?;
                    out = Some(PathBuf::from(text));
                }
                other => return Err(format!("unknown argument {other:?}")),
            }
        }
        Ok(Self { out })
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
        assert_eq!(args.forwarder.map(|address| address.port()), Some(25640));
        assert_eq!(args.tee.port(), 25641);
        assert!(!args.real_client, "a live session logs in");
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
    fn export_gamedata_arguments_parse() {
        let args = ExportArgs::parse(["--out", "x/y.json"].map(str::to_owned)).unwrap();
        assert_eq!(args.out, Some(PathBuf::from("x/y.json")));
        assert_eq!(ExportArgs::parse(std::iter::empty()).unwrap().out, None);
        assert_eq!(
            ExportArgs::parse(["--out"].map(str::to_owned)).unwrap_err(),
            "--out wants a value"
        );
        assert!(ExportArgs::parse(["--bogus"].map(str::to_owned)).is_err());
    }

    #[test]
    fn operators_are_repeatable_and_default_to_none() {
        let base = [
            "--forwarder", "127.0.0.1:1", "--tee", "127.0.0.1:2", "--goal", "g", "--run-dir", "r",
        ];
        let args = parse(&base).unwrap();
        assert!(args.operators.is_empty(), "no operator, no chat");
        assert_eq!(args.chat_prefix, "p:", "tasks need `p:` unless configured");
        let mut with = base.to_vec();
        with.extend(["--operator", "Fatih", "--operator", "Ayse"]);
        assert_eq!(parse(&with).unwrap().operators, vec!["Fatih", "Ayse"]);

        // Decision 2026-11-09: the task prefix is configurable; empty means
        // every operator line is a task.
        let mut custom = base.to_vec();
        custom.extend(["--chat-prefix", "görev:"]);
        assert_eq!(parse(&custom).unwrap().chat_prefix, "görev:");
        let mut none = base.to_vec();
        none.extend(["--chat-prefix", ""]);
        assert_eq!(parse(&none).unwrap().chat_prefix, "");
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
    fn real_client_mode_needs_no_forwarder() {
        let args = parse(&[
            "--real-client",
            "--tee",
            "127.0.0.1:2",
            "--run-dir",
            "r",
            "--port",
            "127.0.0.1:3",
            "--name",
            "Fatih",
        ])
        .unwrap();
        assert!(args.real_client);
        assert_eq!(args.forwarder, None, "shadow mode never logs in");
        assert_eq!(args.name, "Fatih", "the name is the operator's player");
    }

    #[test]
    fn the_hands_flags_are_one_mode_for_real_client_only() {
        let base = [
            "--real-client",
            "--tee",
            "127.0.0.1:2",
            "--run-dir",
            "r",
            "--port",
            "127.0.0.1:3",
        ];
        let mut full = base.to_vec();
        full.extend([
            "--hands",
            "hands/macos/build/praxion-hands",
            "--client-pid",
            "1234",
            "--client-options",
            "options.txt",
            "--hands-activate",
            "--hands-look-latency",
            "3",
        ]);
        let args = parse(&full).unwrap();
        assert_eq!(
            args.hands,
            Some(PathBuf::from("hands/macos/build/praxion-hands"))
        );
        assert_eq!(args.client_pid, Some(1234));
        assert_eq!(args.client_options, Some(PathBuf::from("options.txt")));
        assert!(args.hands_activate);
        assert_eq!(args.hands_look_latency, 3);

        // The latency has a default and the modifier flags are optional.
        let mut minimal = base.to_vec();
        minimal.extend(["--hands", "h", "--client-pid", "1", "--client-options", "o"]);
        let args = parse(&minimal).unwrap();
        assert_eq!(args.hands_look_latency, 2);
        assert!(!args.hands_activate);

        // The three paths are all or nothing.
        let mut partial = base.to_vec();
        partial.extend(["--hands", "h"]);
        assert_eq!(
            parse(&partial).unwrap_err(),
            "--hands, --client-pid and --client-options are required together"
        );

        // A modifier alone is not a mode.
        let mut modifier = base.to_vec();
        modifier.extend(["--hands-activate"]);
        assert!(parse(&modifier).unwrap_err().contains("--hands-activate"));

        let mut bad_pid = base.to_vec();
        bad_pid.extend(["--client-pid", "many"]);
        assert!(parse(&bad_pid).unwrap_err().contains("process id"));

        // A logged-in body has no hands.
        let mut logged_in = [
            "--forwarder",
            "127.0.0.1:1",
            "--tee",
            "127.0.0.1:2",
            "--goal",
            "g",
            "--run-dir",
            "r",
        ]
        .to_vec();
        logged_in.extend(["--hands", "h", "--client-pid", "1", "--client-options", "o"]);
        assert!(parse(&logged_in).unwrap_err().contains("--real-client"));
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
