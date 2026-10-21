# Fast decision models: TypeSafe Jev and classifier LLMs

Research date: 2026-10-21. `[V]` primary source, `[VENDOR]` vendor's own claim,
`[3P]` third-party measurement, `[U]` not verified.

## Why this matters

A frontier LLM takes seconds per call; Minecraft ticks every 50 ms. Moment-to-moment
choices (next behaviour, which skill, is the goal done, is this dangerous, which command did
the player mean) need a model that answers a bounded question in ~10–300 ms.

## TypeSafe Jev

- **What:** a "System One" model. Input: state (text/JSON) plus typed questions. Output: typed
  answers with probabilities, in one parallel pass. It never generates text. [V]
- **Primitives:** `Choice` (≤255 options, probabilities), `Score` (ordered levels),
  `Noul` (P(yes)), `Box` (experimental, 1.14). Many questions per request. [V]
- **Versions and price:** `jev-1.13.0`, `jev-1.14.0` (2026-10-14). $0.042 per 1M input tokens,
  output free; 64k context. API `POST /v1/systemone`, Python and JS SDKs, also on OpenRouter.
  No fine-tuning. Pin versioned IDs; aliases move. [V]
- **Latency:** vendor 70–500 ms; third parties measured ~130–320 ms, Cloudflare measured 524 ms.
- **Weak at:** counting, arithmetic, multi-hop questions, long irrelevant state; state text can
  steer answers (prompt injection); first-option bias (disputed by one paper). [V]
- **Games:** official Doom demo on text state (~10 decisions/s; vendor admits a scripted bot plays
  better). Community Minecraft repos (rmalde, teknium) pair a frontier planner with Jev picking
  bounded actions through Mineflayer: Ender Dragon in ~8 min on Peaceful with known routes. [V/3P]

## Open alternatives (all 2026)

| Model | Kind | Size / license | Latency | Notes |
|---|---|---|---|---|
| Cloudflare Clef / Clef-Flash [V] | Decision head on Qwen3.5 (27B / 9B), multimodal | Apache-2.0 | Flash: 38.8 ms median [VENDOR]; 0.5–1 s on an iGPU [3P] | Jev wire-compatible (`/v1/systemone`) |
| Laya [V] | Encoder (ModernBERT-large / mmBERT) + decision heads | 421M / 322M, Apache-2.0 | 147 ms for 1 question on Apple MPS [3P] | Near chance zero-shot; good after fine-tuning; overconfident; weak above ~20 options |
| GLiNER2.5-Decide [V] | DeBERTa-v3 encoder | 340M, Apache-2.0 | | JSONL fine-tuning built in; probability output unconfirmed |
| Liquid AI d1-3B [V] | Small decoder decision model, MLX builds | 3.1B, LFM license (not Apache) | 30 ms on M5 Pro [VENDOR]; 640 ms at 3.4k-token state | Best on-device candidate on paper |
| SemIf [V] | Option-token logprobs from frozen Qwen3.5-4B | MIT + Qwen license | ~1 s | Simplest DIY approach |

**Classifier-LLM technique:** any open LLM can act as a decision model by reading the
probabilities of the option tokens in one forward pass. What it lacks is calibration: fit a
temperature per question type on our own labelled decisions.

## Third-party head-to-head (System1Doom, ViZDoom)

Jev, Laya and Clef 27B succeeded 90–95% on the one scenario that separated models; SemIf and
Clef-Flash 55%. In most scenarios the hand-written controller dominated, and a random policy
matched the models on some. Lesson: the controller and the question design matter more than
the model choice.

## Design implications

1. Code enumerates legal options and computes geometry, counts and distances; the model only judges.
2. Gate on calibrated probabilities, not argmax; low confidence falls back to rules or the big LLM.
3. Ask when the decision changes, not on every state change. Keep state small.
4. Treat player chat as an injection surface: allowlists and checks in code.
5. One backend-agnostic `/v1/systemone` client lets us swap Jev, Clef, Laya.
6. Log every decision with its outcome. That dataset is what makes fine-tuning an open model possible later.

## Sources

Jev launch https://typesafe.ai/blog/introducing-system-one-models-and-jev · Docs https://docs.typesafe.ai/llms.txt ·
Models https://docs.typesafe.ai/models · Limits https://docs.typesafe.ai/model-jaggedness/jev-1.13 ·
Jev 1.14 https://typesafe.ai/blog/jev-1-14 · OpenRouter https://openrouter.ai/typesafe/jev-1.13 ·
Option rotation https://arxiv.org/abs/2609.37647 · Coherence https://arxiv.org/abs/2609.33209 ·
Persuasion https://arxiv.org/abs/2609.31142 · Benchmark https://github.com/nibzard/decision-model-benchmark ·
rmalde https://github.com/rmalde/minecraft-agent · teknium https://github.com/teknium1/hermes-and-jev-play-minecraft ·
souta-lab https://github.com/souta-lab/minecraft-jev-agent · Clef https://huggingface.co/Cloudflare/clef-flash ·
Clef blog https://blog.cloudflare.com/clef-decision-models · Laya https://huggingface.co/convaiinnovations/laya ·
GLiNER2.5-Decide https://huggingface.co/fastino/GLiNER2.5-Decide · Liquid d1 https://huggingface.co/LiquidAI/d1-3B ·
SemIf https://github.com/TheoLeeCJ/SemIf-OpenJev · System1Doom https://github.com/PanikParty/System1Doom
