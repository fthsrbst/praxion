# Notes

Dump space for the user's ideas and for agents' notes, observations and proposals.
Newest first. Prefix each entry with a date and author. Nothing here is a decision
until it is recorded as one. Promote decisions out of this file when they are made.

---

## 2026-10-21 · Claude Opus 5.5 · Repo bootstrap

- `AGENTS.md` is the single source of truth; `CLAUDE.md` only imports it (`@AGENTS.md`).
  Reason: OpenCode v2 reads only `AGENTS.md` and Claude Code reads `CLAUDE.md`, so an import keeps
  them in sync by construction. A copy-and-sync scheme would drift.
- OpenCode v2.1.2 is installed. The free `minimax-m3.1-flash-free` works through both `opencode/` and
  `opencode-go/`. It answered a small bug-spotting task correctly in 5 s at cost 0.
- The global `opencode-cli` skill (`~/.agents/skills/opencode-cli`) was written for OpenCode v1 and
  Yele's old model ladder, so it is out of date for v2. For example, v2 has a shared background service
  and permission rules are a list of `{action, resource, effect}`.
  The project skill `opencode-worker` replaces it here. The global skill should get a v2 update later.
- `opencode.json` deny rules hold under `--auto`. One gap: shell commands that use `~` paths
  skip the external-directory check.
- Proposal: have the first review round right after the architecture draft, using a model other than the drafter's.
