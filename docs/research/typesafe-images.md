# TypeSafe images and Jev 1.14 (read 2026-11-01)

Sources: https://docs.typesafe.ai/images, https://docs.typesafe.ai/primitives/box,
https://typesafe.ai/blog/jev-1-14. Skill installed: `.claude/skills/typesafe-ai/` (MIT, from
`typesafe-ai/skills`, guidance only, no scripts). Status per TypeSafe: "Dreamer-ready", i.e. for
prototyping; request format, answers and limits may change between releases.

## Images

- Not a separate model: images are an input to System One (`POST https://api.typesafe.ai/v1/systemone`).
  Jev 1.13 and 1.14 accept them.
- **Every request that carries an image or a Box question must send `TypeSafe-Danger-Zone-Images: true`**,
  otherwise `400`. Praxion's Jev client sets this header on such requests only.
- PNG or JPEG stills, listed once in a top-level `assets` map (`type: "image"`, `part` for multipart or
  `data_base64` + `format`), referenced by key from `state`, `instructions` or `criteria`. Every question
  type accepts images.
- Limits: 280 to 1568 px per side (out-of-range images are rejected, not resized), 78,400 to 2,458,624
  pixels, 16 MiB per image, 32 MiB per request. A 1920x1080 Minecraft frame must be scaled to at most
  1568 px wide before sending.
- Cost: billed as input tokens at the model's normal price; no separate image rate limit. Latency: not
  published. TypeSafe says all models are weaker on images than on text.

## Box (new question type, Jev 1.14 only)

- `{"type": "box", "instructions": "the coffee mug in `photo`"}` returns `box{top, bottom, left, right}`
  as fractions (origin top-left) plus `confidence`.
- Always returns a box, never "not found": pair it with a Noul ("Is there a mug in `photo`?").
  One target per question; small targets in big images: locate a region, then ask again on a crop.

## Jev 1.14

- Available as `jev-preview`; `jev-latest` moves to `jev-1.14.0` about a week after the post.
- Small gains across the board, largest in multilingual input (matters for Turkish chat commands).
- New `box` type. Long context behind `TypeSafe-Danger-Zone-Long-Context: true`. Opt-in query sharing
  (not used for training, per TypeSafe).
- Deprecation notice: 60 days for `jev-1.13.0`, 30 days for `jev-1.14.0`. No accuracy, latency or
  price figures published.

## Where images fit in Praxion (proposals, not decisions)

Session 3 decided: no screenshots in the decision loop; the world comes from the protocol. So images
belong in tests and tooling, not in play.

1. **Line-of-sight oracle (Demo-0/1).** For recorded frames, ask Nouls such as "Is a zombie visible in
   `frame`?" and compare with what the perception API said was visible at that tick. Measures our
   line-of-sight walk (transparent blocks are still [O]) against what a player actually sees.
2. **Screen-state checks for level-3 tests (Demo-1, E10/E11).** Choice "which screen is open in `frame`:
   game, pause, inventory, chest, death, chat" and Box "the slot holding the stone pickaxe" to verify
   that menu clicks at GUI scales 1 to 4 landed where intended. An independent oracle next to the
   proxy stream.
3. **Run reports.** Flag frames worth a human look (death, odd camera motion) before the naturalness
   blind test; the blind test itself stays human.
4. **Commands with a screenshot (later).** The user could send "build here" with a screenshot; Box gives
   the point. Changes a session 9 decision, so only if the user wants it.

## Elsewhere (user's other projects)

- **TIRPAN (authorized pentest agent):** triage of web-service screenshots during recon (Choice: login
  page, default install page, admin panel, error page; Noul: "shows a version string"), and Box to
  locate a login form for browser automation in lab targets.

## Smoke test (2026-11-01, user's key, from the Windows PC)

`jev-latest` already resolves to `jev-1.14.0`.

| Request | Result | Latency | Input tokens |
|---|---|---|---|
| Text Noul, Turkish chat "bana biraz demir topla" → asks to collect? | 0.98 | 777 ms | 293 |
| Box + Noul on a synthetic 800x600 PNG, header set | box l .61 r .75 t .24 b .42 (truth .625/.75/.25/.417), conf 0.95; Noul 0.98 | 514 ms | 1575 |

One run each; no latency claim beyond "sub-second on this PC once". A real Minecraft frame is untested.
