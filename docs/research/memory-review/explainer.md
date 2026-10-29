# Memory for Praxion, explained from zero

A beginner's walk-through of the first memory proposal ([memory-world-state.md](../memory-world-state.md)).
2026-10-27. Nothing here is a decision; recommendations are mine and each states its main alternative.

**Tags.** `[V]` = read in a primary source this session (path/URL inside the bracket). `[U]` = my own
knowledge, unchecked. `[S]` = secondary source. Web access was used for three sources only: the RAG
paper, the Minecraft wiki on chunks, the wiki on zombies; toy embedding numbers are invented.

## 1. What "memory" means for a Minecraft agent

Praxion's eyes are a read-only proxy that copies traffic between the real game client and the private
local server [V docs/decisions.md]. Through it the agent knows the blocks around the player, the
inventory, health and the mobs (creatures) in range, exactly, for the loaded area.

That snapshot is **live state**: the agent's eyes. A **tick** is one game step, 1/20 s [U], and the
live state is replaced 20 times per second.

**Live state is not memory.** Five reasons:

1. **It has no past.** The client holds "the block at (120, 64, 300) is a chest". It does not hold
   "it was air at 10:00, I placed it, then someone moved it".
2. **It is partial.** The game loads only chunks near the player, and unloaded chunks are unprocessed
   [V https://minecraft.wiki/w/Chunk]. A chunk is a 16x16 column of the world. "No chest in the loaded
   area" is a fact about the window, not the world.
3. **It is overwritten, not revised.** When the chest moves, the old value is gone; you cannot ask
   why, when, or who.
4. **It dies on restart.** Close the game and the agent remembers nothing.
5. **It cannot hold a lesson.** "I died in lava at y=12" is an event; "do not dig straight down into
   lava at y=12" must be derived and stored somewhere.

So **memory is the part of the agent's knowledge that survives the current tick, carries when and how
it was learned, and can be wrong in a way the agent can notice and repair.**

| Question | Where the answer lives |
|---|---|
| "What is in my hand right now?" | Live state |
| "Where did I sleep last night?" | Journal of past events |
| "Where is the nearest chest with iron?" | Belief store, then a live check |
| "How do I make an iron pickaxe?" | Static game data (a recipe) |
| "What did I learn about creepers?" | Lessons |

**About "instantly stale to current".** A fact can be refreshed instantly only if it is observable
now. If nobody looked at the chest for twenty minutes, no system can know it moved; the honest answer
is the old value plus its age plus a plan to check. §6 lists where the proposal blurs this.

## 2. RAG and semantic search, with a toy example

- **Embedding**: a model turns text into a list of numbers, a **vector** (say 1024 numbers). Similar
  meanings get similar vectors. Each number alone is meaningless to a human [U].
- **Cosine similarity**: one number from -1 to 1 measuring the angle between two vectors; 1 is "same
  direction", so "very similar". It is used to rank rows [U].
- **Vector index**: finds the nearest vectors without comparing every row. With a few thousand
  lessons, comparing all rows is fast enough [V memory-world-state.md:103].
- **RAG** (retrieval-augmented generation): the 2020 paper defines it as combining a model's internal
  knowledge with an external memory — a **dense vector index** — that a retriever searches before the
  model answers [V https://arxiv.org/abs/2005.11401]. In practice: search a corpus, paste the top
  passages into the prompt, answer from them.
- **Keyword search (BM25)**: ranks by word overlap, not meaning [U].

**Toy example.** Pretend the model returns three numbers per sentence: (place-ness, item-ness,
danger-ness). Real embeddings have 1024+ dimensions with no such labels; the axes are a teaching
fiction.

| Row | Text | Vector |
|---|---|---|
| F1 | Chest 7 at x=120, z=300 holds 32 iron ingots | (0.90, 0.80, 0.00) |
| F2 | Chest 8 at x=150, z=310 holds 32 iron ingots | (0.88, 0.82, 0.02) |
| F3 | Creepers explode when close: hit once, then step back | (0.10, 0.10, 0.90) |
| F4 | Only on Hard difficulty can a zombie break a wooden door | (0.30, 0.20, 0.60) |

Query A is "where is my chest at x=120" → (0.90, 0.70, 0.00). Query B is "what did I learn about
creepers" → (0.12, 0.10, 0.88).

| Query | Row | Score | Verdict |
|---|---|---|---|
| A | F1 | 0.998 | the right chest, but the stale row |
| A | F2 | 0.996 | wrong chest, practically tied |
| B | F3 | 0.997 | right lesson, found although the words differ |
| B | F1 | 0.180 | irrelevant |

The model sees "chest + iron" in both chest rows. A coordinate is a few characters in a sentence, and
one vector holds the whole sentence, so 30 blocks of difference barely moves it [U]. Similarity cannot
tell current from stale or one chest from another. The right tool is a key lookup:
`container(x=120, z=300)` → "broken at tick 27890, replaced by chest 8 at 150, 310". Exact, current.

Query B is the opposite case: similarity is right, and keyword search would miss the same lesson
asked as "the green thing that explodes" [U].

| Question shape | Right tool |
|---|---|
| Exact key: coordinates, counts, chest contents | Typed lookup, no embeddings |
| "All facts about X" | A filter; similarity returns "most X-like", not "all X" [U] |
| "What did I learn / what happened" | Filter by kind/area, then BM25 + embedding, ranked |
| Wiki-style static knowledge | RAG fits; recipes and item stats are tables and a graph, not RAG |

**Short answer as a proposal:** keep exact, changing facts in a database with time stamps, and use
semantic search only for text-shaped knowledge: lessons, episode descriptions, wiki pages. That is the
proposal's central claim [V memory-world-state.md §2], and I agree [U]. The main alternative is one
text log of everything plus RAG over it: simpler, decent for "what happened", but it cannot answer
exact or current questions. I would try it as an experiment, not as the design.

## 3. The proposal, piece by piece, in one story

| Piece | What it is | What it is not |
|---|---|---|
| Live state | In-memory snapshot of what the client sees now | Memory; it has no past |
| Event journal | Append-only list of observations, actions, chat, deaths | A summary; nothing is overwritten |
| Belief store | Facts projected from the journal by plain code: key, value, observed, source, age, superseded_by (the fact that replaced it) | The truth; or a chat log |
| Lessons | Short sentences learned from episodes, embedded, with reliability | Facts about coordinates |
| Static game data | Recipes, block and item tables, mob rules, from Mojang data | Learned, or time-sensitive |
| Resolver | Code answering "what do we know about X, right now" | An LLM |
| Briefing | The small text handed to Jev (fast hosted decision model) or the LLM (large language model) | The whole memory |
| Freshness | Per-kind rules for age, authority (which source wins) and refresh | One global time limit |

```
 real client ──► PROXY ──► LIVE STATE ───────────────┐  "now", in memory, overwritten
                    │                                │
                    ▼                                │
              EVENT JOURNAL (append-only)            │
                    │ projection (plain code)        │
       ┌────────────┼─────────────┐                  │
       ▼            ▼             ▼                  │
  BELIEF STORE   LESSONS   STATIC GAME DATA          │
       └────────────┼─────────────┘                  │
                    ▼                                │
  question ──► RESOLVER ─────────────────────────────┘
                    │ refresh what is visible; label the rest with age
                    ▼
                BRIEFING ──► Jev / LLM
```

**The running story.** Difficulty is Normal [V docs/decisions.md]. The agent mines iron, puts 32
ingots in a chest at (120, 64, 300), keeps 4, walks 300 blocks west. Night comes; a zombie finds the
agent and breaks nothing. Next morning the user takes over, walks back, moves the chest to
(150, 64, 310), puts the 32 ingots back, hands control over and says "bring me iron". The proxy keeps
recording during the hand-over: eyes and hands are separate.

| Piece | T0 — deposit (t=4000) | T1 — away, then night (t=9000+) |
|---|---|---|
| Live state | pos (120,64,301); 4 iron ingots; block (120,64,300) is a chest | pos (-180,70,300); base chunk unloaded, so nothing about the chest |
| Journal | `t=4000 place_chest 120,64,300`, `t=4001 deposit 32 iron` | walking; `t=13500 zombie_spotted`, `t=14020 damage 2`, `t=14100 zombie_lost`; no block events |
| Belief store | chest#7 @ (120,64,300) = 32 iron; source sight; age 0 | same value, 5000 ticks old (about 4 minutes), remembered, not live |
| Lessons | — | candidate, tag `inferred`: "at night a capped 1x1 hole is safe from zombies" [U] |
| Static data | chest and iron pickaxe recipes [U] | a zombie breaks a wooden door only on Hard [V https://minecraft.wiki/w/Zombie] |

Something alarming happened and nothing in the world changed, so no fact changed; the journal still
keeps it. "What happened" and "what is true" are different questions.

| Piece | T2 — the user moves it (t=27900) | T3 — the question (t=28000) |
|---|---|---|
| Live state | the user walked back, so the chunk loaded: chest, air, a chest at the new spot, a window with 32 iron | pos (150,64,312); 4 iron; block at the new spot is a chest; contents not visible |
| Journal | `t=27890 break 120,64,300`, `t=27930 place 150,64,310`, `t=27971 contents 32 iron` | the chat message |
| Belief store | chest#7 `valid_to=27890`, `superseded_by=chest#8`; chest#8 @ (150,64,310) = 32 iron | chest#8 = 32 iron, age 29 ticks (1.5 s), source sight |
| Resolver | — | inventory → live; containers → typed query plus spatial index (lookup by coordinates); follow supersession |
| Lessons | — | the zombie lesson is irrelevant, so it is not sent |

Nothing sends a "chest moved" event; the code sees a break and a place. Contents are known only
because the user opened the chest — an assumption ("contents arrive only then") the proposal marks
unverified [V memory-world-state.md:68]; §6 flags it.

```
GOAL:  user said "bring me iron" (chat, t=28000)
NOW:   pos 150,64,312; hp 20/20; inv 4 iron ingot; chest block at 150,64,310
KNOWN: chest#8 (150,64,310) holds 32 iron
       observed 29 ticks ago, source sight (window), confidence high
OLD:   chest#7 (120,64,300) broke at t=27890, superseded by chest#8
ASK:   take iron from chest#8, bring it to the user
```

A chunk is 16x16 blocks and 384 tall, 98,304 blocks [V https://minecraft.wiki/w/Chunk]. Sending that
to an LLM is impossible; this six-line briefing is the point of the design.

**T4 — the action, tick 28300.** Jev or the LLM picks chest#8. The skill walks there, opens it, takes
20 ingots, brings them. Journal: `t=28300 withdraw 20 iron chest#8`; belief store: chest#8 = 12 iron;
live state: 24 iron. Success is the inventory delta (the change in count), not "the code stopped"
[V systems-review.md:265].

**Resolver and freshness.** Five small steps per question: (1) turn the question into fact keys; (2)
read live state where possible; (3) read the belief store otherwise; (4) refresh what is observable
and journal the change; (5) label the rest with age and source. Plain code, so it is testable.

| Fact kind | Where it lives | What happens on a read |
|---|---|---|
| Inventory, health, position | Live only | Never stored |
| Block in a loaded chunk | Live | Refreshed; a change becomes a journal event |
| Block in an unloaded chunk | Belief + age | Labelled stale; verified on arrival |
| Container contents | Belief + age | Only opening the window refreshes it [U] |
| Named place | Belief, long | Checked on approach; dropped if the anchor block is gone |

## 4. Glossary

1. **Chunk** — a 16x16 column of the world, the unit the game loads and unloads [V wiki Chunk].
2. **Tick** — one game step, 1/20 s; 20 per second [U].
3. **Live state** — what the client sees now; overwritten every tick.
4. **Memory** — knowledge that outlives the tick and carries age and source.
5. **Event journal** — append-only list of what happened; never edited.
6. **Projection** — plain code that builds current facts from the journal.
7. **Fact (belief)** — one typed statement with key, value, observation time and source.
8. **Provenance** — where a fact came from: sight, hearing, memory, inference, testimony [V systems-review.md:267].
9. **Age / freshness** — how long ago a fact was observed; stale is not false.
10. **Supersede** — mark an old fact invalid and link the new one; never delete.
11. **Resolver** — code that turns a question into live reads and labelled memories.
12. **Briefing** — the short text actually sent to the model.
13. **Embedding (vector)** — a list of numbers representing meaning.
14. **Cosine similarity** — a score from -1 to 1 for how similarly two vectors point.
15. **RAG** — retrieve passages by similarity, then answer from them.

## 5. Self-check

1. **Why not call the live snapshot "memory"?** No past, only the loaded window, overwritten rather
   than revised, lost on restart, and it cannot hold a lesson.
2. **Two chests, (120,300) and (150,310). Why does embedding search fail on "where is my chest at
   x=120"?** Both sentences embed almost identically; the coordinate is a tiny part, so the scores
   nearly tie and the stale row can win. A key lookup plus supersession answers it.
3. **What does the journal hold that the belief store does not?** Raw events, including ones that
   changed nothing (the zombie), with exact times and order. The belief store is derived and current.
4. **The agent saw 32 iron in chest#8 ten seconds ago and the chunk is loaded. Can the resolver
   confirm the count without opening the chest?** No. Loading proves the block is a chest, not what is
   inside; contents arrive with a window [U]. It must open it or send the value with its age.
5. **Which is honest: "the chest is at 120" or "the chest was at 120, twenty minutes ago"?** The
   second. "Unknown" is not "absent": the agent must be able to say it does not know.

## 6. Where the proposal is confusing or weakly argued

1. **Read-through refresh over-promises, and its key assumption is unverified.** It treats "chunk
   loaded" as "observable", but container contents need an action (open the window), which costs time
   and can fail. The freshness table says "until next open"; the proposal's §4 rule says "refresh if
   loaded". And the whole chest story depends on "contents arrive only when opened"
   [V memory-world-state.md:68], still marked `[U]`; a packet capture in session 2 should settle it.
2. **Push invalidation has a blind spot.** The proxy sees block-change packets only for chunks the
   client has loaded [U]. A change made while nobody was there leaves no packet, yet the doc promises
   contradiction "the moment it happens, with no polling" without that condition.
3. **Chunk loading is a burst nobody budgets for.** One chunk is 98,304 blocks [V wiki Chunk], so
   journaling per block is impossible; the unit must be a chunk snapshot. The doc never says this and
   never mentions Baritone's 2-bit chunk cache, about 16 KiB per chunk [V systems-review.md:247],
   which is the obvious storage for block memory.
4. **Object identity is missing.** The schema links facts (`superseded_by`), not objects. A moved
   chest, a renamed item, a rebuilt wall: the code sees break + place and cannot know it is "the
   same" thing. The story above papers over exactly this.
5. **"Two time stamps" is muddled; freshness is per kind only.** The sketch has three time fields,
   and "when it was true" versus "when we learned it" is never separated; tick versus wall clock is
   never justified (ticks stop when paused, sleeping jumps them) [U]. Two facts of one kind can also
   differ in trust — seen versus heard — and there are no TTL (time-to-live) numbers and no retention
   policy.
6. **Lesson scoring and refutation.** Recency decay (from Generative Agents) suits episodes; "a
   zombie cannot break a door on Normal" does not get less true with age. Importance "assigned by
   code rules" is unspecified [V memory-world-state.md:91–93], and nothing can switch off a wrong
   lesson except its score drifting down.
7. **The briefing has no contract.** No token budget, no ordering rule, no rule for what is dropped
   when it does not fit. "Give the LLMs the correct context" is the user's requirement, and this is
   the piece that meets it.

## Sources

- Proposal `docs/research/memory-world-state.md`; decisions `docs/decisions.md`; review `docs/research/systems-review.md`; embeddings `docs/research/embeddings.md` [V]
- RAG https://arxiv.org/abs/2005.11401 · Chunks https://minecraft.wiki/w/Chunk · Zombies https://minecraft.wiki/w/Zombie [V]
