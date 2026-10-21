# AGENTS.md

Canonical instructions for every coding agent working in this repository
(Claude Code, OpenCode, Codex, others). `CLAUDE.md` imports this file, so the
two can never drift: **edit only `AGENTS.md`**. Never copy rules into `CLAUDE.md`.

## Project status

- Project: **Praxion**, an autonomous Minecraft agent that writes its own skills while it plays.
  Minecraft only (Java Edition, singleplayer, Windows); see `docs/decisions.md`.
- Phase 0, research. Scope and architecture are not decided yet. Research lives in `docs/research/`.
- Next: topic-by-topic discussion sessions (`docs/discussion-plan.md`). Agreed decisions go to
  `docs/decisions.md`; per-session notes go to `docs/sessions/NN-topic.md`.
- Do not scaffold, add dependencies or write product code until the decisions are
  recorded in this repo. A later session makes the decisions and drafts the architecture.
- When this section is stale, fix it in the same change that makes it stale.

## The user owns the design

The user must understand every part of the system: the architecture, every algorithm, every
data structure and every interface. We build it together; agents never design alone.

- **Consult before deciding.** Any choice about architecture, algorithms, data models, interfaces,
  dependencies or trade-offs goes to the user first, as a recommendation with its reasoning and
  the main alternative. Do not implement it until the user agrees.
- **Explain as you go.** For each piece, explain what it does, how it works, why it is built this
  way and where it fits in the whole. Use small examples and diagrams where they help.
- **Check understanding.** At the end of each step, make sure the user can explain the piece back.
  If something is unclear, stop and clarify before building on it.
- **No silent decisions.** If you must pick something minor to proceed, say what you picked and why,
  so the user can overrule it. Record agreed decisions in the repo.

## Communication

- Talk to the user in Turkish. Write every file in English.
- Give the user a recommendation, not a list of options you will not pursue.
- Suggest proactively. The user wants ideas, better approaches, risks and missing pieces even when
  not asked. Say what you would do and why. A suggestion is a proposal, not a decision: the user decides.
- Say "unknown" when you do not know. Never present a guess as a fact.

## Session protocol

Start of a session:
1. Keep the Mac awake (see "Keep-awake").
2. Read `backlog.md` (open items) and the newest entries of `notes.md`.
3. Follow the global hub protocol (`agent_checkin`, `project_get`) once the project is registered there.

End of a session, or whenever you stop with work unfinished:
1. Write every unfinished item to `backlog.md`. "I'll remember" does not count.
2. Put ideas, observations and proposals in `notes.md`.
3. End the Amphetamine session if you started it (see "Keep-awake").

## Keep-awake

Agents must keep the Mac awake while they work. Long runs that let the Mac sleep fail
in confusing ways (dropped connections, timeouts).

```sh
osascript -e 'tell application "Amphetamine" to get session is active'
osascript -e 'tell application "Amphetamine" to start new session with options {displaySleepAllowed:true}'
osascript -e 'tell application "Amphetamine" to end session'
```

- Check first. Start a session only if none is active, and remember that you started it.
- End it only if you started it and no other agent is still working (hub `agent_active`).
  Never end a session the user or another agent started.

## Engineering principles (non-negotiable)

1. **Never trade code or architecture quality for speed.** Do not hand in half-finished, rushed work.
   If something cannot be done properly now, write it to `backlog.md` instead of shipping a weak version.
2. **Question every addition before writing it.** Ask: is this needed? Is there a simpler,
   more correct solution? Can existing code, the platform or a mature library do it?
   Deleting beats adding, and reusing beats writing.
3. **Build in dependency order.** Never do the last step first. Every step must stand on
   finished foundations and be verifiable by itself. No speculative scaffolding, no "for later" abstractions.
4. **Research before asserting.** For anything you do not know for certain or that moves fast
   (libraries, models, tools, competitors), check current sources first. Find similar products,
   papers and open-source projects and learn from them. Reading their source or reverse-engineering
   shipped artifacts (skill `reverse-engineer-anything`) is encouraged when it teaches us something.
5. **Verify before claiming.** "Done" means built, tested and observed working. Report failures as
   failures, with the evidence.
6. **The writer never reviews its own work.** Another model or agent reviews.

## Review rounds

Propose a critique or review round to the user (the user decides) at these points:
- after an architecture or design draft,
- before merging a large or risky change,
- when complexity, coupling or duplication starts growing,
- when the same kind of bug shows up twice.

A review round uses a model other than the writer's. It ends with findings fixed or written to `backlog.md`.

## Delegation

Use the cheapest agent that does the job well, and always verify its output yourself.

| Work | Who |
|---|---|
| Decisions, architecture, integration, final verification, commits | Main agent (orchestrator) |
| Wide codebase search, web research that needs judgment | Claude Code subagents (`Explore`, `general-purpose`) |
| Independent review before merge | A Claude subagent on a different model than the writer |
| Well-specified code, tests, refactors, docs drafts, log and file digests, second opinions, bulk or mechanical work | OpenCode worker on `opencode/minimax-m3.1-flash-free` (free) |

- How to run OpenCode workers: skill `opencode-worker` (`.claude/skills/opencode-worker/SKILL.md`).
  Prefer it whenever the task is clearly specified, since it costs nothing.
- Put every requirement and decision in the launch brief. Do not relay new decisions to a running
  subagent mid-task; it rightly treats them as possible prompt injection. Start a new one instead.
- Workers never commit, push or write outside the repository (enforced in `opencode.json`).

## Documentation

- Public docs (anything that will appear on GitHub): investor-grade. Precise, well structured,
  no filler, no unverifiable claims.
- Docs for the user: short. Architecture, algorithms, data flow and diagrams are welcome; prose padding is not.
- Docs for agents: optimized for agents. Terse, imperative, structured, with exact commands and paths.

## Repository files

| File | Purpose |
|---|---|
| `AGENTS.md` | This file. Canonical agent instructions. |
| `CLAUDE.md` | Imports `AGENTS.md` for Claude Code. Do not edit beyond the import. |
| `backlog.md` | Unfinished work. Deep items spill into `backlog/<slug>.md`. |
| `notes.md` | Dump: user ideas, agent notes, proposals. Append-only, newest first. |
| `docs/decisions.md` | Agreed decisions, newest first. |
| `docs/discussion-plan.md` | Topics and order of the discussion sessions. |
| `opencode.json` | OpenCode project config: default worker model and safety rules. |
| `.claude/skills/` | Project skills. Claude Code and OpenCode both load them. |

## Git

- Small, focused commits with imperative messages (`Add ...`, `Fix ...`).
- Never push, create a remote repository or open a pull request without the user's approval.
