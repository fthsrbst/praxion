# Session 15: Infrastructure and operations

Decided 2026-10-29. Inputs: decisions 1.2 (layering enforced by tooling), 2.5 (brain and body
apart), 6 to 14, backlog "Choose a license".

## Fixed by earlier sessions
The body runs on the Windows PC (real mouse and keyboard); the proxy and the local server listen on
loopback only; the brain may run elsewhere, default on the same PC (2.5); the only network
dependence is the brain (Jev and LLM calls); the wiki index and embeddings are local (session 12);
languages are Python and Rust (2.6); cost is tracked and the user is warned (session 8).

## Decided (user, 2026-10-29)
- **The brain runs on the same Windows PC by default.** Simplest for the demo: one machine,
  loopback, fewest moving parts. A brain on the Mac or elsewhere stays possible (2.5) but is not
  the default. Rejected for now: a cloud brain.
- **One repository, with the layers enforced by tooling.** A Rust workspace for the body (proxy,
  decoder, hands, world model, rule-list interpreter), a Python package for the brain, docs, the
  test harness and the hand-written core skills. Agent layers may not import body layers (decision
  1.2); the build or a lint rule fails otherwise. `spikes/` stays local. Rejected: separate repos.
- **Keys live in the operating system credential store** (Windows Credential Manager or macOS
  Keychain), with a `.env` file outside the repo as a fallback. Only the brain holds keys; the body
  holds none; keys never reach logs or error messages. Remote brain access needs its own key (2.5).
- **Licence: Apache-2.0 for the open core, commercial parts closed (open core).** The `LICENSE` file
  (the canonical Apache-2.0 text from apache.org) is added to this repository and the README is
  updated. Which parts count as commercial is not decided yet.

## Minor picks by Claude (user may overrule)
- **Test budgets:** nightly and level-2/3 test runs get a hard token and cost budget that stops
  the run; play and the demo only warn (session 8).
- **Secrets hygiene:** `.env` and any key file are in `.gitignore`; a pre-commit check refuses
  strings that look like keys.
- **Process isolation:** the skill writer (session 11) runs as a separate low-privilege process;
  the exact Windows mechanism is chosen when it is built.
- **Server:** a vanilla server jar on the PC with Java 25 (Minecraft 26.3). The server EULA is
  accepted by the user, never by an agent.
- **Dependencies:** only licences compatible with the open core (MIT, Apache-2.0, BSD); copyleft
  libraries (Baritone LGPL, ZenithProxy AGPL, Cairn AGPL) stay ideas only.
- **Wiki index:** stays outside the repository and is never distributed (session 12).

## Left open, with owner
- Which parts are commercial and closed: with the business model.
- The Windows isolation mechanism for the skill writer and the exact `LICENSE` notices in source
  files (headers): at the first code.
- Cloud or CI machines for nightly tests: when nightly tests exist.
