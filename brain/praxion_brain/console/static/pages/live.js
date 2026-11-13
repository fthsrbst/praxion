// The Live page: mission, thought, plan, body, surroundings, inventory and the feed.
import { el } from "../util.js";
import { store, onState, onEvidence } from "../store.js";
import { createFeed } from "../feed.js";
import { createMission, createThought, createPlanStrip, createVitals, createSurroundings } from "../panels.js";
import { renderInventory } from "../inventory.js";

export function mount(ctx) {
  const feed = createFeed(el("feed"), { open: ctx.open });
  const mission = createMission(el("hero"));
  const thought = createThought(el("thought"));
  const planStrip = createPlanStrip(el("plan"));
  const vitals = createVitals(el("vitals"));
  const surroundings = createSurroundings(el("surroundings"));

  /** When the running goal's command was sent, from the feed's own cards. */
  function startedAt(goal) {
    if (!goal) return null;
    const items = feed.timeline.items;
    for (let i = items.length - 1; i >= 0; i -= 1) {
      if (items[i].type === "goal" && items[i].goalId === goal.goal_id) return items[i].t0;
    }
    return null;
  }

  function render(s) {
    mission(s, startedAt(s.goal));
    const plan = feed.timeline.plan;
    thought(s.thought || (plan && plan.thought) || null, plan && plan.thought === s.thought ? plan.ts : null);
    planStrip(plan, s.plan);
    vitals(s);
    surroundings(s.nearby);
    renderInventory(s.facts, s.snapshot_at);
  }

  feed.load(store.evidence);
  onEvidence((entry) => feed.add(entry));
  onState(render);
  feed.onChange(() => render(store.state));
  render(store.state);
  return { feed, show() { feed.refresh(); } };
}
