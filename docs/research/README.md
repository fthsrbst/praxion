# Research

Phase 0 research for Praxion (2026-10-21). Nothing here is a decision yet.

| Topic | File |
|---|---|
| Skill-writing game agents, general game agents, Minecraft interfaces and benchmarks | [game-agents.md](game-agents.md) |
| Fast decision models: TypeSafe Jev and classifier LLMs | [decision-models.md](decision-models.md) |
| Embeddings and rerankers: Gemini Embedding 2, Qwen | [embeddings.md](embeddings.md) |

## The picture in one page

1. **Skill libraries work, but they are Minecraft-only and weakly verified.** Voyager (2023)
   started the idea; Programmatic Skill Networks (2026) made skills a self-repairing graph and
   reached diamond 2x faster. Nobody has a game-agnostic skill layer with real tests per skill.
2. **Speed is the central constraint.** LLM calls take seconds; games need answers in
   milliseconds. Every working system splits a fast loop (code, reflexes, a small decision model)
   from a slow loop (LLM planning and skill writing). Thinking only at key moments beats
   thinking every step.
3. **Decision models fill the middle layer.** Jev (hosted, ~130–300 ms, ~$0.0003 per decision)
   and new open models (Clef, Laya, Liquid d1, GLiNER2.5-Decide) answer bounded questions with
   probabilities. They are weak at arithmetic and counting, and every accuracy number is
   vendor-run, so we must benchmark on our own decisions.
4. **Embeddings are a commodity, retrieval design is not.** Qwen3-Embedding runs free and local;
   Gemini Embedding 2 adds images, video and audio in one space. Skill retrieval needs structured
   filters plus reranking, not embeddings alone.
5. **Minecraft tooling is ready.** Mineflayer is active, MIT and supports up to MC 26.1.

## Open questions for the architecture phase

- What exactly is a "skill" (code, graph node, contract with pre/postconditions)?
- How is a skill verified, versioned and rolled back?
- Which decisions go to code, which to a decision model, which to the LLM?
- Hosted or on-device decision model; how do we collect labels to calibrate it?
- How far does the game-agnostic core go before the Minecraft demo?
- How do we run LLM-written code safely?
