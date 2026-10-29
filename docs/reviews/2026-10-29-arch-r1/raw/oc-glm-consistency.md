<!-- Reviewer: GLM-5.3-Flash via OpenCode opencode v2.1.2. Lens: glm-consistency. Raw, unverified output. -->

Now let me read all session notes to cross-check against the architecture.
Now let me check `backlog.md` and `notes.md` for cross-references, and verify a few fast-moving external facts via web search.
Web search was cancelled; let me try a direct fetch of the Mojang version manifest to verify the 26.3 baseline fact.
26.3 verified as newest stable. Let me quickly verify the Azalea decoder claim (the project's main library risk) and the protocol-version number.
Two quick fact-checks to close the loop on the version/protocol baseline:
### Findings

**ID**: CONS-1
**Severity**: blocker
**Where**: `docs/architecture.md` §2 ("Process boundaries") and §11 risk 3
**Claim**: The architecture reopens a decided decision by marking "whether the forwarder is its own OS process or an isolated thread inside the body" as **[O]** with "the isolated thread as the fallback", but session 2.2's D1 — recorded verbatim in `decisions.md` — decided a **separate process**, user-confirmed, on 2026-10-29.
**Evidence**: `decisions.md` 2026-10-29: "Proxy: a separate process that forwards bytes untouched and decodes a copy." `sessions/02` §2.2: "D1: separate proxy process... Rejected: terminate both connections and re-encode." The "isolated thread" fallback appears nowhere in any session note; both the [O] tag and the fallback are the writer's invention. This also makes §11 risk 3 ("the extra hop from forwarder to body") ambiguous — the extra hop is a property of the decided design, not an experiment outcome.
**Recommendation**: Restate in §2: forwarder is its own OS process **[D]** (source: D1, 2.2). Delete the thread fallback, or keep it only as an explicitly user-reopened [P]. Rephrase risk 3 as "verify E1 that the decided extra hop keeps within budget", not as an open design question.
**Confidence**: high

**ID**: CONS-2
**Severity**: major
**Where**: `docs/architecture.md` §4 row "Interface window"
**Claim**: The row says "not decided **[O]**" for the whole interface window, but the interface's existence and responsibilities are decided; only the technology is open.
**Evidence**: decision 2.4 [D]: "indicator: a small separate window outside the game area, never stealing focus"; decision 1.5 [D]: "Hand-over can start three ways: in-game chat command, a global hotkey... and a button in a separate interface"; decision (session 9) [D]: "The separate interface is the window of 2.4 with a text box." The repo-layout section itself says "interface/ window (technology open)" — the correct split.
**Recommendation**: Split the row: requirements **[D]** (2.4, 1.5, 9, 14), technology **[O]**. As written, a reader could reopen decisions about the indicator window in a review round.
**Confidence**: high

**ID**: CONS-3
**Severity**: major
**Where**: `docs/architecture.md` throughout (no coverage section; no traceability)
**Claim**: Several decided behaviours are silently absent from the architecture, so the "consolidates the decisions" claim cannot be checked.
**Evidence**: Missing [D] content: user's projects before Praxion's (1.4); big-risk announcements with ~30 s stop window (session 10); empty hand-over behaviour "resume suspended task, else choose a goal and say so" (session 10); death is acceptable (1.4); the PC is dedicated while playing (1.6 block); human limits without fatigue (~150 ms reaction floor, human aim statistics; session 13); reply language follows the user's, name/tag `Praxion` (session 13); the five success measures of 1.6 (ladder, command suite, smoothness, naturalness, cost) appear nowhere; the wrong-click caught by the container-click slot number (decisions.md, mouse-modes bullet); `.env` credential fallback (session 15); the lesson trigger "a mistake is seen twice" (session 12). Additionally `decisions.md` 2026-10-23 contains an internally ambiguous wording: "Rules ... **proposal:** code enforces them deterministically after the user confirms" — embedded inside a decided bullet, while session 7 and architecture §7 treat code enforcement as [D].
**Recommendation**: Add a §13 "decisions → section" traceability table with one row per `decisions.md` bullet and the architecture section that carries it; bullets with no row must be listed explicitly as gaps. Reword the 1.4 "proposal:" clause in `decisions.md` to a plain statement (the user already confirmed the session-7 treatment).
**Confidence**: high (for the list of omissions); the "user confirmed 1.4 enforcement" reading is medium — only the user can resolve the "proposal" wording.

**ID**: CONS-4
**Severity**: major
**Where**: `docs/architecture.md` §1 and `decisions.md` (whole file)
**Claim**: The architecture pins the version mid-paragraph in a `[D]` sentence, but `decisions.md` has no version decision at all, and the agreed rule is "the newest stable release at the time coding starts" (1.1) — a moving target with 26.4 snapshots already shipping (26.4-snapshot-6, 2026-10-27). Any future release makes the silently implied "26.3 [D]" either stale or in conflict with 1.1.
**Evidence**: Verified externally: 26.3 is the newest stable release (https://piston-meta.mojang.com/mc/game/version_manifest_v2.json, releaseTime 2026-09-15); protocol version 777 and "Minimum Java version Java SE 25" (https://minecraft.wiki/w/Java_Edition_26.3); 26.4 snapshots up to -6 exist. Also session 1's background still claims "Newest full release found: 26.2; 26.3 was in snapshots" — superseded by session 2's update but never corrected in `decisions.md`.
**Recommendation**: Record a dated version-baseline bullet in `decisions.md` ("26.3, protocol 777, Java SE 25, verified 2026-10-23/29") and make the architecture cite it as a checked fact with a verification date, not as part of a [D] paragraph. State explicitly whether the demo follows the *rolling* 1.1 rule or pins 26.3 (see questions).
**Confidence**: high

**ID**: CONS-5
**Severity**: major
**Where**: `docs/decisions.md` 2026-10-29 ("Languages" bullet) vs `docs/sessions/02` §§2.5–2.6 and `docs/architecture.md` §4
**Claim**: The decisions file says "Exact boundary decided in 2.5", but 2.5's own session notes left the port transport open ("Message format and transport are chosen when the first code is written") and 2.6 is recorded as "timing and process boundary open". Meanwhile the component table drafts the whole ~/.10 ms Python/Rust split — which 2.6 marked only as "proposed split rule (to confirm in 2.5)" — as if settled, with no tags.
**Evidence**: `sessions/02` table row 2.6: "Languages decided (Python + Rust); timing and process boundary open"; §2.6: "**Proposed split rule (to confirm in 2.5)** ... The exact boundary (PyO3 module or local socket) is the body port, 2.5"; §2.5 "Left open on purpose": port contract, catalog, evidence levels. `decisions.md`: "Exact boundary decided in 2.5."
**Recommendation**: Fix the `decisions.md` bullet ("boundary *shape* decided in 2.5; wire/transport and the 10 ms split rule confirmed nowhere yet"). In architecture §4, tag every component whose language assignment rests on the unconfirmed split rule (forwarder/decoder/hands etc.) as **[P] until the 10 ms split rule is confirmed** — currently only the memory row carries a [P].
**Confidence**: high on the contradiction; medium on which exact text the user should confirm first.

**ID**: CONS-6
**Severity**: minor
**Where**: `docs/architecture.md` §2 diagram ("Forwarder"), §4 row ("Forwarder"), §5 ("The proxy then sees...")
**Claim**: Two names for one decided component: the architecture's tables say "Forwarder", the decided terminology (D1, and every session note) says "proxy", and §5 introduces "the proxy" without ever defining it. A reader cannot tell whether forwarder = proxy or proxy = forwarder + decoder.
**Evidence**: `decisions.md`: "Proxy: a separate process that forwards bytes untouched and decodes a copy"; all 15 sessions use "proxy". The architecture's own §5 "Action and verification" switches to "The proxy then sees what the client sends" right after §4 named it "Forwarder". Notably the decided E1 measures "latency added by the forwarding **proxy**" (sessions/02 §2.7).
**Recommendation**: Use "proxy" as the component name everywhere in the architecture, and define it once: proxy = forwarding path (bytes untouched) + decoder (reads a copy). Keep "forwarder" only as the sub-part if needed.
**Confidence**: high

**ID**: CONS-7
**Severity**: minor
**Where**: `docs/architecture.md` §5 ("The brain loop")
**Claim**: The event list silently diverges from the session 8 closed list and omits decided event sources from other sessions, although session 8 states the list is closed.
**Evidence**: Architecture: "program finished, monitor fired, skill failed, chat, new threat, take-over ended, ruleset refusal, or a heartbeat". Session 8 [D]: "program exhausted, monitor fired, skill failed, chat, new threat, take-over ended, ruleset refusal" — different first item. Adding to it but absent from §5: stuck/planner unreachable "an event wakes the LLM" (session 6 [D]); "reflex ran long or the world changed a lot → the LLM is woken by an event" (session 7 minor); "a goal finished or failed" (session 10 minor).
**Recommendation**: Use session 8's wording verbatim, then list the additional causes from sessions 6/7/10 as explicit additions pending user confirmation — because "closed list" [D] and unenumerated additions contradict.
**Confidence**: high

**ID**: CONS-8
**Severity**: minor
**Where**: `docs/architecture.md` §5 ("Hand-over and take-over") vs `decisions.md` 2.4
**Claim**: The take-over trigger list is incomplete in the architecture; experiments E12 depend on the complete list.
**Evidence**: Decision 2.4 [D]: "any key **or mouse-button press, wheel turn** or clear mouse movement takes control back". Architecture: "Any physical key or clear mouse move takes control back" — mouse buttons and wheel dropped.
**Recommendation**: Restore the full decided trigger list in §5 (and check the §2 diagram caption).
**Confidence**: high

**ID**: CONS-9
**Severity**: minor
**Where**: `docs/architecture.md` §4 table, §6
**Claim**: Several rows present session *"minor picks by Claude (user may overrule)"* content as decided, while the table header presumes a decided-tinted "Decisions" column; only some rows carry [P].
**Evidence**: "Commands | ... mapped by Jev to a closed schema" cites session 9, where Jev mapping is under **Minor picks**; "Versions live in git with automatic rollback" (§6) overstates session 11's conditional rollback rule (Wilson-bound crossing after ≥ 8 uses, itself a minor pick); "Memory and briefing | Python (files read through the body) **[P]**" introduces an unexplained design ("read through the body") that matches no session text and clashes with the architecture's own risk 10 ("who owns the memory file" open) and with the body-port philosophy ("queries and events", session 3).
**Recommendation**: Give the §4 table its own tag convention and apply it per-cell; tag Jev mapping, automatic rollback, and the watchdog phrase as [P]; rewrite the memory row as "suggested ownership [P]; live state is queried from the body (session 3), file ownership open (risk 10)".
**Confidence**: high

**ID**: CONS-10
**Severity**: minor
**Where**: `docs/architecture.md` §3, §5 ("in under 50 ms")
**Claim**: Latency bounds are presented as settled properties, but the docs hold them at two different statuses that were never reconciled: decision 1.6 says "Target numbers are set after the first baseline measurements", session 2.7 says "thresholds accepted as listed" (p99 < 50 ms), and 2.4 says "Target: take-over under 50 ms".
**Evidence**: `decisions.md` 2026-10-23: "Starting points to test against: ... take-over < 50 ms"; `sessions/02` §2.7: "p99 < 50 ms" (E12); §2.4 minor pick: "Target: take-over under 50 ms".
**Recommendation**: In §5 write "take-over target: p99 < 50 ms (E12, threshold accepted 2026-10-29; 1.6 still calls it a starting point)". One sentence; removes an ambiguity a reviewer could waste a round on.
**Confidence**: high

**ID**: CONS-11
**Severity**: minor
**Where**: `docs/decisions.md` 2026-10-29 ("Experiments (2.7)" bullet) and `docs/sessions/02` §2.7 vs `backlog.md` and `docs/architecture.md` §10
**Claim**: The "spike code lives in untracked `spikes/` **until the licence is chosen**" condition has expired (Apache-2.0 chosen the same day, session 15), but `decisions.md` still carries the expired condition, `backlog.md` explicitly re-opens the question, and the architecture writes "`spikes/` local experiment code (untracked)" as if it were settled — three files, three answers.
**Evidence**: `backlog.md`: "decide whether `spikes/` may be committed now that the licence is chosen"; `sessions/15`: "`spikes/` stays local" (different rationale, no open question mentioned).
**Recommendation**: Decide once (see questions) and align all four files; until then, mark it [O] in the architecture instead of the parenthetical fact-of-life "untracked".
**Confidence**: high

### Ideas (brainstorm)
- **Machine-checkable traceability table** (`decisions` bullet → architecture section): turns CONS-3 from a one-off review into a lint that future agents and the next draft rounds can run mechanically.
- **Name the component "proxy" (decided term) with `forwarder` and `decoder` as sub-parts** so the E1 experiment, the risk table, and the component table stop using two words for one thing; zero design change, pure consistency win.
- **A per-risk "fail path" column in §11**: each row names what stop-and-go look like if its experiments don't pass (per 2.7, any missed threshold stops and goes to the user). Cost guard: prevents a reviewer from mistaking the risk table for a decision table.
- **Version-constants file with a `verified-on` date** (release, protocol PVN, minimum Java) regenerated by a script that hits the Mojang manifest; a new release then surfaces in CI rather than only being noticed in session notes before the next session.
- **Verify the<!— licence nuance:minecraft.wiki technical pages imported from wiki.vg are separately licensed CC BY-SA 3.0 (not NC)** — visible in the licence banner of e.g. https://minecraft.wiki/w/Java_Edition_protocol. Worth citing inConsult the wiki-RAG row before anyone hard-codes "undistributable" for the whole corpus; it may only block the gameplay pages.
- **One timing diagram of a take-over event through hook → arbiter → hands** unifying the pieces that live in 2.4, session 7, and session 4 — a single artifact a reviewer can eyeball against the 50 ms budget instead of three overlapping sections.
- **Model the "registry/port journal" for receipts as the actual data flow** (client packet → server ack → effect observed) in §5, so multi-session concepts like "settle windows" and "sequence-number attribution" have a shared vocabulary instead of per-session footnotes.
- **Collect all quality gates (18/20 first nights, 100% reflex cells, no A-B-A within 5 s, false `confirmed` 0, red-team 0/20)** in one "gates" table with source citations — the same numbers are currently scattered in sessions 7/8/11/14.

### Questions for the user
1. **Proxy process**: is the isolated-thread fallback a re-opening you want, or a mistake to be corrected back to decided D1 (separate proxy process)?
2. **Version baseline**: does the demo pin 26.3 explicitly (accepting the 26.4 snapshot risk), or does it follow the rolling "newest stable release at coding start" rule from session 1? Should we record this as a decisions bullet so the architecture can stop guessing?
3. **spikes/**: may they be committed now that the licence is chosen (Apache-2.0)?
4. **Language split rule** ("anything with a deadline under ~10 ms is Rust", session 2.6's proposal marked "to confirm in 2.5"): do you want to confirm it now, or should the architecture tag every affected component row [P] until you do?
5. **Jev mapping commands to a closed schema** (session 9 minor pick): promote to a decided bullet in `decisions.md`, or keep it open until session 8's measurements land?

FINAL_REPORT — The three most important findings: (1) CONS-1, the architecture marks the decided "separate proxy process" as **[O]** and invents an "isolated thread" fallback that no session ever discussed — this reopens a user-confirmed decision and makes the risk table ambiguous; (2) CONS-3, roughly a dozen decided behaviours (user projects first, 30 s big-risk announcement, resume-then-choose on empty hand-over, human-reaction limits, reply language/tone, the five 1.6 success measures…) are silently absent from the architecture with no traceability mechanism to notice; (3) CONS-5, the decisions file claims "Exact boundary decided in 2.5" while 2.5 explicitly left the port transport open — so the whole Python/Rust component split is drafted on an unconfirmed "~10 ms" rule and unpinned by tags. Fixing these three requires no new design work; it is alignment work that must be done before the user reviews [P]/[O] items, otherwise those reviews will be hijacked by already-decided questions.
