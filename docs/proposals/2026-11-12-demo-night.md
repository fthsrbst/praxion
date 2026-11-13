# Demo night: plan queue, wider tasks, camp, voice, wiki RAG (2026-11-12)

Status: decided by the user on 2026-11-12 (four choices, all the recommended ones). This file is the
shared contract of the parallel work items; each item has its own brief in `logs/demo1112/`.

## Why (measured on 23 recorded runs, `local/brain/brain.jsonl`)

- One proposal call per goal: `gpt-6-luna` p50 7.7 s, p90 11.2 s (`gpt-6.1-terra` p50 3.1 s). The body
  stands still during every call.
- 108 600 ticks (about 90 minutes of play) were spent in `nothing_to_do` back-off: `rate_limit` x33
  (the governor allows 30 proposals an hour), `governor_code_only` x18, `proposals_excluded` x6.
- The proposal prompt says "prefer the first unmet ladder rung", so every run plays the same order.
- Jev scored the helper skill `cut_log` equal to `get_wood` (0.91 / 0.93) and the helper won once
  (run A, seed 584146: `get one log` failed `no_block` after 7 s).
- A task such as "köy bul" is refused by design: a goal must be an inventory condition.
- The snapshot carries no surroundings, so the model cannot react to what is near.

## Decisions

1. **Plan queue.** One model call returns a short plan (1 to 5 goals, in order) plus one sentence of
   thinking. The loop plays the queue goal by goal; code validates every goal when its turn comes. The
   plan is dropped and made again after a failed goal, a death, a survival interruption, a user task or
   when it runs empty. The ladder stays as the yardstick shown in the console; it is no longer a
   required order.
2. **Models.** Planner `gpt-6.1-terra`; after two invalid plans in a row, or for the first plan after
   a death, `gpt-6.1-sol`. The command map call uses `gpt-6.1-terra`. The writer is unchanged.
3. **Variety.** Each run draws one persona from a fixed list (seeded by the world id) and the planner
   sees it together with the surroundings; the planner chooses the order itself.
4. **Jev.** Still routes a goal to a skill, but only over public skills (helpers are hidden), and a
   skill the planner named plays when Jev scores it at or above 0.5.
5. **Wider tasks.** An operator task may name a known skill with arguments and no inventory condition
   (`find village`, `go to a block`, `hunt`); such a goal is verified by the skill's own `done`.
6. **Camp.** New core skills place a crafting table, furnace, chest and torches at one spot; no new
   build primitive tonight.
7. **Voice.** ElevenLabs in the web console: narration lines are spoken, and a held key records the
   operator's speech, which is transcribed and sent down the existing task path.
8. **Wiki RAG.** A fixed list of about 300 minecraft.wiki article pages, fetched slowly with an
   identifying user agent, indexed locally only (never distributed), local multilingual embeddings.

## Shared interfaces (do not change without updating this file)

### Snapshot facts: `nearby` (body -> brain, additive, optional)

```json
"nearby": {
  "entities": {"cow": 2, "zombie": 1},
  "blocks": {"coal_ore": 3, "oak_log": 12, "water": 40},
  "sounds": ["entity.creeper.primed"]
}
```

- `entities`: count per entity kind (no namespace) within 32 blocks of the eyes, from the perception
  API (so the same fairness rules as every other fact apply).
- `blocks`: count per block id of *notable* known blocks within 16 blocks: logs, ores, water, lava,
  crafting_table, furnace, chest, bed, bell, hay_block, torch. Omitted ids count zero.
- `sounds`: distinct sound ids heard within 16 blocks in the last 100 ticks, newest first, at most 8,
  without the `minecraft:` prefix.
- The whole key is omitted when empty. The brain's `port.Facts` gains
  `nearby: Optional[Nearby] = None` with three fields defaulting to empty.

### Planner output (`brain/praxion_brain/planner.py`)

```python
class PlannedGoal(BaseModel):
    text: str                      # short imperative sentence
    condition: str                 # guard text; "" only for a skill-verified goal
    skill: Optional[str] = None    # an exact known public skill name, or None
    args: list[str] = []           # the skill's arguments, as text
    rationale: str = ""            # <= 200 chars, logged

class Plan(BaseModel):
    thought: str                   # <= 200 chars, Turkish, narrated in chat and the console
    goals: list[PlannedGoal]       # 1..5
```

### Operator command plan (`brain/praxion_brain/commands.py`)

`CommandPlan` and `Resolution` gain `args: list[str]`. A resolution of kind `goal` either has a
verifiable inventory condition (as today) or names a known skill (`skill` set, `condition == ""`),
which the loop sends without a condition.

### Wiki search (`brain/praxion_brain/wiki/`)

```python
def search(query: str, k: int = 3) -> list[Snippet]   # [] when no index exists; never raises
class Snippet(BaseModel): title: str; text: str; url: str; score: float
```

### Voice (`brain/praxion_brain/voice.py`, console routes)

- `POST /api/voice/tts` body `{"text": str}` -> `audio/mpeg` bytes (ElevenLabs), 503 when no key.
- `POST /api/voice/stt` body: the recorded audio bytes (`Content-Type` of the recording) ->
  `{"text": str}`.
- `GET /api/voice/status` -> `{"enabled": bool, "voice_id": str}`.
