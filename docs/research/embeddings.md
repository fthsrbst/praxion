# Embeddings: Gemini Embedding 2 and Qwen

Research date: 2026-10-21. `[V]` primary source, `[U]` not verified.

## Where embeddings would serve us

Skill retrieval (goal → candidate skills), memory retrieval (episodes, places, lessons),
near-duplicate skill detection, and later cross-modal retrieval of frames for pixel games.

## Gemini Embedding 2 (`gemini-embedding-2`) [V]

- GA 2026-04-22 (preview 2026-03-10), Gemini API and Vertex.
- One shared space for **text, image, video, audio and PDF**. 100+ languages.
- 128–3072 dimensions (Matryoshka; 768/1536/3072 recommended). 8,192-token input limit;
  per request ≤6 images, ≤120 s video (32 frames), ≤180 s audio.
- No `task_type` parameter; the task goes into the prompt (`task: search result | query: ...`).
- Price per 1M tokens: text $0.20, image $0.45, audio $6.50, video $12 (batch half). Free tier exists.
- No official MTEB number published. Not compatible with `gemini-embedding-001` vectors.

## Qwen models (all Apache-2.0) [V]

| Model | Released | Dim / context | Score |
|---|---|---|---|
| Qwen3-Embedding 0.6B / 4B / 8B | 2025-06 | 1024 / 2560 / 4096, 32K | MTEB multilingual 64.3 / 69.5 / 70.6 |
| Qwen3-VL-Embedding 2B / 8B | 2026-01 | 2048 / 4096, 32K, text+image+video | MMEB-V2 73.2 / 77.8 |
| Qwen3-Reranker 0.6B / 4B / 8B | 2025-06 | 32K | MTEB-R 65.8 / 69.8 / 69.0 |
| Qwen3-VL-Reranker 2B / 8B | 2026-01 | | MMEB-v2 retrieval 75.2 / 79.2 |

- Matryoshka output and instruction prompts supported.
- Local on a Mac: official GGUFs (llama.cpp), Ollama `qwen3-embedding` (0.6b is 639 MB),
  community MLX builds. No published Apple Silicon latency: we must measure. [U]
- Nothing newer than 2026-01 found.

## Qwen as a classifier [V]

- **Qwen3Guard** (0.6B/4B/8B) is the only dedicated Qwen classifier, and it is a **content-safety
  moderator** (Safe / Controversial / Unsafe), not a general task classifier.
- **Qwen3-Reranker** is effectively a yes/no classifier: relevance = P("yes") vs P("no").
- Any Qwen LLM can classify by comparing label-token logprobs or with a fine-tuned classification
  head. This is the same technique the open decision models in [decision-models.md](decision-models.md) use.

## Hosted vs local

| | Gemini Embedding 2 | Qwen local |
|---|---|---|
| Modalities | Text, image, video, audio, PDF | Text; VL models add image + video |
| Cost | Cheap for text; frames get expensive (~$0.0008 each) | Free, uses local memory |
| Latency | Network round trip | No network; unmeasured |
| Privacy / offline | No | Yes |

## Design implications

1. Hide the model behind one `Embedder` interface; store model id, dimension and prompt format
   with every vector, and keep raw text/frames so we can re-embed after a switch.
2. Text retrieval can start local and free (Qwen3-Embedding 0.6B or 4B); measure latency first.
3. Use Gemini Embedding 2 when cross-modal retrieval matters (frames, audio).
4. Embeddings alone are not enough for skills: structured filters first (game, preconditions,
   success rate), then embedding top-k, then a reranker or LLM check on the top few.
5. Dedup by high cosine similarity, confirmed by a reranker or judge, since near-identical skills
   can differ in preconditions.

## Sources

Gemini launch https://blog.google/innovation-and-ai/models-and-research/gemini-models/gemini-embedding-2/ ·
GA https://blog.google/innovation-and-ai/models-and-research/gemini-models/gemini-embedding-2-generally-available/ ·
Model page https://ai.google.dev/gemini-api/docs/models/gemini-embedding-2 ·
Guide https://ai.google.dev/gemini-api/docs/embeddings · Pricing https://ai.google.dev/gemini-api/docs/pricing ·
Qwen3-Embedding https://huggingface.co/Qwen/Qwen3-Embedding-8B · GGUF https://huggingface.co/Qwen/Qwen3-Embedding-0.6B-GGUF ·
Qwen3-VL-Embedding https://github.com/QwenLM/Qwen3-VL-Embedding · paper https://arxiv.org/abs/2601.04720 ·
Ollama https://ollama.com/library/qwen3-embedding · Qwen3Guard https://github.com/QwenLM/Qwen3Guard
