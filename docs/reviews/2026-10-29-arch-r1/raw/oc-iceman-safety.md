<!-- Reviewer: Iceman Contributor Free via OpenCode opencode v2.1.2. Lens: iceman-safety. Raw, unverified output. -->

### Findings

- **ID**: SAF-TAKEOVER-1
- **Severity**: blocker
- **Where**: `docs/architecture.md` §2, `docs/sessions/02-body-connection.md` §2.4, `docs/sessions/04-action-verification.md` §"Safe take-over"
- **Claim**: The "release all keys on take-over" rule has an exception ("unsafe_until" moments hold keys) but the race between user input and the hold logic is underspecified. Combined with "take-over in under 50 ms" and hook-based detection, a misread of physical vs injected input could either fail to release keys (leaving injected input active) or incorrectly hold them.
- **Evidence**: Session 2.4 says take-over ignores tiny jitter and releases only keys Praxion holds that the user does not physically hold; session 4 adds `unsafe_until` where keys it needs stay held until the user touches that key or any movement key. Hook uses `LLKHF_INJECTED`/`LLMHF_INJECTED` (inferred/observed) but driver-level injectors can look physical (session 2.1 notes). No stated tie-break: if user presses a key at the exact moment the hold is about to be released, or if the hook misses the first physical event due to `LowLevelHooksTimeout` (docs say hook removed on timeout), the state table can desync.
- **Recommendation**: Treat the "user physically holds any key/button/wheel/clear movement" as an absolute preempt that forces release of all injected keys immediately regardless of `unsafe_until`, with a "never hold against a physical key the user is actively pressing" invariant. Make the hold set intersection-tested against the live physical key table on every tick, require the hook to be on a dedicated thread with timeout monitoring, and add a fail-safe: if hook health is lost, release all keys and stop. Also log the physical/injected source for every held key in receipts.
- **Confidence**: high

- **ID**: SAF-INPUT-2
- **Severity**: blocker
- **Where**: `docs/sessions/02-body-connection.md` §2.1, §2.3
- **Claim**: Reliance on `SendInput` + scan codes + distinguishing injected vs physical input is not robust across Windows states (RDP, elevated game, DPI, focus, toggle keys). The design explicitly rejects driver injectors to preserve the injected flag, but the injected flag is not guaranteed to be trustworthy and some environments break it.
- **Evidence**: Session 2.1 notes injected events may have `hDevice = 0` (observed) but GLFW ignores it; `SendInput` blocked by UIPI if game is elevated; RDP blocks raw relative input; `pauseOnLostFocus` releases mouse grab; toggle vs hold differs by `options.txt`. Experiments E12 require watchdog to release keys within 1s of kill, but no stated defense if the input thread is suspended or the watchdog itself is killed.
- **Recommendation**: Treat "cannot distinguish safely" as a hard failure mode: at startup detect elevation/RDP/remote desktop/session type and refuse with a clear message (don't proceed). Treat loss of hook health as equivalent to take-over. For toggle keys, model state explicitly and never assume hold. Add a "keys held > N ticks with no brain/body heartbeat" invariant that forces release. The watchdog must be a separate process with its own watchdog or at least a session that cannot be terminated by the main process in the same user session without detection.
- **Confidence**: high

- **ID**: SAF-PROXY-3
- **Severity**: blocker
- **Where**: `docs/architecture.md` §2, `docs/sessions/02-body-connection.md` §2.2, §2.7
- **Claim**: The "read-only proxy that never encodes packets" is the right structural choice, but decoder desync, compression differences, and the 26.3 Azalea support are unverified. A bad decoder could mark perception invalid at the wrong time or miss packets. Also "the game is unaffected" relies on never re-encoding - correct in principle, but operationally a stuck proxy/decoder could stall the copy and break timing assumptions.
- **Evidence**: Azalea 26.3 branch last pushed 2026-10-16 (unverified), MCProtocolLib is fallback, E1-E3 not run, compression `network-compression-threshold` proposal noted. Session 2.2 says "A decoder that loses sync marks perception invalid and Praxion stops safely; the game is unaffected." - stopping safely is good, but "stops safely" must include releasing all injected keys and not leaving the agent in a half-action state.
- **Recommendation**: Treat decoder loss of sync as a "fail closed" event that triggers immediate key release + reflex disable + brain notification, with a backoff before reconnect. Measure proxy p99 < 1ms (E1) before any input path is enabled. Have a recorded packet capture regression set for 26.3. Also ensure the forwarding path is truly never encoding (code review + tests) and that the copy is delivered without blocking the forward path.
- **Confidence**: high

- **ID**: SAF-CHAT-4
- **Severity**: blocker
- **Where**: `docs/architecture.md` §5, §7; `docs/decisions.md` (2026-10-23/29 chat rules)
- **Claim**: "Chat is an injection surface: only the user's player is obeyed; free text never reaches a planner as an instruction" is stated as decided, but the design lacks concrete enforcement details (packet source filtering, prefix mapping, schema validation, and prevention of cross-player chat reaching the command parser).
- **Evidence**: Commands come from in-game chat with a prefix and from the interface (decision 9). "Ambiguous commands: low risk guess/start/ask without blocking; high risk ask first" - but if source spoofing is possible or if other players can chat, the filter must be strict. The proxy sees all packets; "only the user's player is obeyed" requires knowing the user's player UUID/name and rejecting other senders. No mention of rate limiting, or of chat packets that could be used to confuse the parser.
- **Recommendation**: Enforce sender allowlist by the user's player entity/UUID at the packet level (before any parsing). Chat without the prefix and from non-user sources must be logged but never parsed as commands; never pass raw free text to Jev/LLM as instructions. High-risk commands require confirmation even if prefixed. Add tests for cross-player chat injection.
- **Confidence**: high

- **ID**: SAF-RULESET-5
- **Severity**: major
- **Where**: `docs/architecture.md` §7, `docs/decisions.md` "Ruleset" (2026-10-29)
- **Claim**: Ruleset is "written with an LLM, confirmed by the user, enforced by code before any input" but the trust model is unclear. If the LLM can generate rules, there must be a hard allowlist/denylist that cannot be modified by the brain/LLM at runtime, and enforcement must apply to reflexes too (noted) but also to every action path including skill primitives.
- **Evidence**: Decision says "written with an LLM, confirmed by the user, enforced by code before any input" - confirmation is good, but storage format, signing, and tamper resistance not specified. Session 7 says "Ruleset enforcement: protected blocks, items and regions are checked in code before any input is sent; it applies to reflexes too." - correct, but what if the ruleset file is edited? Or if brain tries to override?
- **Recommendation**: Ruleset must be loaded from a file owned/read-only to the body process (or signed), with hash verification at load time. Enforcement must be in the body (Rust) with no network dependency, applied in the same place for reflexes, arbiter, skills, and primitives. The brain can propose changes but cannot enforce them; changes require user confirmation and are written to the ruleset only through a controlled path. Add deny-by-default for operator-like actions.
- **Confidence**: high

- **ID**: SAF-SECRETS-6
- **Severity**: major
- **Where**: `docs/architecture.md` §7, `docs/decisions.md` "Keys" (2026-10-29), `docs/sessions/15-infrastructure.md` (implied)
- **Claim**: Keys in OS credential store, held only by brain, never logged; remote brain needs its own key. But brain<->body communication is local by default; if brain runs on another computer (allowed), the body port needs auth. Also "only the brain holds keys" means if brain is compromised, keys are exposed - but the boundary says body must survive brain crash; remote access expands attack surface.
- **Evidence**: Decision 2.5 says "Brain may run on another computer. Default is this PC only (loopback); network access needs an explicit key." Architecture says "hosted Jev and LLMs (the only network dependence)". No details on mutual auth, encryption, or how the explicit key is used for body-brain link vs API keys.
- **Recommendation**: Treat body-brain as a security boundary: if remote, require mutual TLS with pinned certs or a pre-shared key, and the explicit key must not be the same as API keys. API keys must never be sent to body. Body must authenticate brain commands (owner tokens exist) but need to prevent command injection if brain is compromised. Log all network connections and refuse remote by default. Keys must never appear in event logs/receipts.
- **Confidence**: high

- **ID**: SAF-SKILLWRITER-7
- **Severity**: major
- **Where**: `docs/architecture.md` §6, `docs/decisions.md` "Skills" (2025-10-29), `docs/sessions/05-skills.md`, `docs/sessions/11-skill-learning.md`
- **Claim**: "The skill writer is a low-privilege process that can write only to a candidates directory" but admission/compilation details are vague. LLM writes rule lists only, compiler derives guards from primitive effects - good. But if compiler runs with elevated privileges or can load arbitrary code, or if admission can be bypassed, it's risky.
- **Evidence**: Session 5 says LLM may only combine existing primitives and skills; compiler derives guards. Session 11 says staged admission. No mention of sandboxing the compiler, or of preventing the skill writer from writing outside candidates even if brain is compromised.
- **Recommendation**: Skill writer process must run as low privilege, chroot/jail to candidates dir only (OS-level), with no network. Compiler must run in same restricted context or in body with read-only access to primitives/skills registry. Admission must be enforced by body (Rust) not just Python brain - body must validate rule lists against registry before interpreting. Red-team the admission gate as specified in backlog.
- **Confidence**: high

- **ID**: SAF-PERCEPT-8
- **Severity**: major
- **Where**: `docs/architecture.md` §5, `docs/decisions.md` "Seeing" (2026-10-29), `docs/sessions/03-perception-world-model.md`
- **Claim**: Line of sight policy is good for fairness, but "everything else is denied and written to the ledger" and "F3 screen shows is allowed" need strict enforcement. Also "out of sight a query answers `unknown`" - reflexes must not use denied data. The world model lives in body (good) but provenance tracking must be enforced.
- **Evidence**: Session 3 says hidden data goes to ledger, privileged reads logged. But if a primitive or reflex accidentally uses raw chunk data, it could bypass policy. Also hearing is allowed within ~16 blocks - need to ensure it's treated as provenance `hearing` not `sight`.
- **Recommendation**: Enforce perception policy in the body at the query API level - deny by default, allowlist only what passes LoS+F3 rules. Every fact must carry provenance tag; actions/decisions must be able to filter by provenance. Reflexes must use only allowed facts. The ledger must be append-only and readable by user but not exfiltrated. Add tests that verify denied data cannot influence actions.
- **Confidence**: high

- **ID**: SAF-BRAINSTOP-9
- **Severity**: major
- **Where**: `docs/architecture.md` §5, `docs/decisions.md` "If the brain goes silent..." (2026-10-29), `docs/sessions/02-body-connection.md` §2.5
- **Claim**: "If the brain goes silent, the body stops safely and tells the user: finishes the current step to a safe point, stands still with basic survival reactions on" - "safe point" is underspecified. A skill in the middle of placing blocks, bridging, or in combat could be dangerous to stop mid-action.
- **Evidence**: Session 2.5 says silence declared after heartbeat gap (~3s), body finishes step to safe point, stands still, keeps basic survival reactions on. Session 4 says dangerous moments hold keys until condition passes/user touches - but if brain goes silent during such a moment, who decides "safe point"?
- **Recommendation**: Define "safe point" as: release all injected keys unless in `unsafe_until` with an active physical user hold, cancel current skill with `preempted`, disable non-reflex actions, enable only reflexes (survival) with highest priority, stop movement, and notify user. Never complete a world-changing action that wasn't confirmed. If in doubt, stop immediately. Add tests for brain crash during each action class.
- **Confidence**: high

- **ID**: SAF-LOGS-10
- **Severity**: minor
- **Where**: `docs/architecture.md` §7, `docs/decisions.md` "Privacy" (2026-10-29)
- **Claim**: "Recordings of the user's play and all logs stay local" but event log is append-only and may contain sensitive info (coordinates, chat from user, items). Also "every claim cites a receipt id" - need to ensure no secrets in receipts.
- **Evidence**: Privacy says stay local. Keys must never be logged. Chat is injection surface - user chat should be logged but need to be careful not to log credentials. No mention of log rotation/size limits or what happens if disk fills.
- **Recommendation**: Treat logs as sensitive local data. Never log API keys, tokens, or full chat that might contain secrets. Add log redaction. Ensure logs aren't sent anywhere. Consider size limits and user control over retention.
- **Confidence**: medium

### Ideas (brainstorm)

1. **"Safe mode" on any security anomaly** - On hook loss, decoder desync, ruleset load failure, or unknown sender chat, drop to reflexes-only with all keys released and require explicit user re-enable. *Why*: fail closed is better than continuing in unknown state.
2. **Physical key shadow table with hardware timestamps** - Track each key with last physical event time from low-level hook; reject any injected action that would conflict with a key pressed within last N ms by user. *Why*: stronger defense against misattributing injected as physical.
3. **Packet allowlist at proxy** - Only forward packets needed for Play state; drop/ignore others, and log drops. Combined with read-only copy, this reduces attack surface. *Why*: least privilege on the wire.
4. **"Panic key"** - A hardcoded or user-configurable key combo that immediately releases all keys and kills input injection regardless of state. *Why*: user needs an escape hatch even if agent is misbehaving.
5. **Action capability tokens** - Instead of just owner tokens, each action needs a capability token scoped to (action class, region, time window) signed by ruleset; body rejects unsigned/expired capabilities. *Why*: prevents brain from issuing arbitrary commands even if compromised.
6. **Deterministic replay gate for skills** - Before admission, replay skill on recorded fixture with perturbed inputs (reflex injection, take-over) and require bit-exact safe behavior. *Why*: catches race conditions in admission.
7. **"Air gap" for secrets** - Store API keys in a separate process/vault that signs requests without exposing raw keys to brain process memory in a readable form? Or use short-lived tokens. *Why*: limits blast radius if brain is compromised.
8. **Per-action "danger score"** - Compute danger score (height, lava nearby, bridging, etc.) and require stricter verification/take-over behavior. *Why*: makes the unsafe_until exception more principled.

### Questions for the user

1. **Elevated game / UIPI**: Will you ever run Minecraft as administrator or on a system where UIPI blocks SendInput? If yes, is a client-mod fallback acceptable, or must Praxion refuse?
2. **Remote brain**: Is running the brain on another computer a requirement for the first demo, or just a future option? If required, what auth mechanism do you prefer (mutual TLS, pre-shared key)?
3. **Ruleset authority**: Should the ruleset be signed by you (cryptographic signature) so only your confirmation can change it, even if the LLM proposes changes?
4. **Panic behavior**: Do you want a physical panic key combo that works even if Praxion is frozen/crashed in some way, and should it require admin privileges?
5. **Chat trust**: In singleplayer with a local server, only you are on the server - is cross-player chat even possible? If you ever play on a LAN server with others, should Praxion still only obey your player (yes) and how do you want to be notified of other players' chat?

FINAL_REPORT The top three findings are SAF-TAKEOVER-1 (race conditions in the key-hold exception to "release all keys" combined with injected/physical detection), SAF-INPUT-2 (environmental brittleness of SendInput/injected-flag detection on Windows), and SAF-CHAT-4 (chat injection surface requiring strict sender allowlisting and parser hardening). All three are blocker-level safety issues that must be resolved before enabling input injection or running the first demo.
