// The Live page's panels: mission hero, thought, plan strip, vitals, surroundings,
// plus the ladder and spend cards the Run page reuses.
import { h, icon, svg, money, num, span, words, sentence, relative, STALE_SECONDS } from "./util.js";

const STATE_LABEL = {
  idle: ["Idle", "dot"], picking: ["Choosing a goal", "brain"], routing: ["Picking a skill", "route"],
  playing: ["Playing", "play"], starting: ["Starting", "dot"],
};
const ORIGIN_LABEL = { you: "your task", survival: "survival", fallback: "fallback", plan: "planned", direct: "" };
const TICKS_PER_SECOND = 20;

// -- honest progress --------------------------------------------------- //
/** The inventory count a `count(item) >= n` goal is chasing, or ``null``.
 *
 * Only a count goal has a real fraction. Every other condition (has(), a night
 * wait, a skill-verified goal) is indeterminate: the bar must not pretend.
 */
export function countProgress(condition, inventory) {
  const match = /^\s*count\(\s*(#?[a-z_]+)\s*\)\s*>=\s*(\d+)\s*$/.exec(String(condition || ""));
  if (!match || !inventory) return null;
  const need = Number(match[2]);
  if (!need) return null;
  const item = match[1];
  let have;
  if (item.startsWith("#")) {
    const pattern = { "#logs": /_(log|stem)$/, "#planks": /_planks$/ }[item];
    if (!pattern) return null;
    have = Object.entries(inventory).filter(([name]) => pattern.test(name)).reduce((sum, [, n]) => sum + Number(n), 0);
  } else {
    have = Number(inventory[item] || 0);
  }
  return { have, need, item: item.replace("#", ""), fraction: Math.max(0, Math.min(1, have / need)) };
}

function chip(text, cls = "") {
  return h("span.chip" + (cls ? "." + cls : ""), { text });
}

// -- mission hero ------------------------------------------------------ //
export function createMission(root) {
  const view = {
    chip: h("span.chip"), chipIcon: h("span.chip-ico"), chipText: h("span"),
    persona: h("span.chip.persona", { hidden: true }),
    learning: h("span.chip.learning", { hidden: true }),
    tick: h("span.chip.quiet"), ladder: h("span.chip.quiet"),
    goal: h("h1.mission-goal"), meta: h("p.mission-meta"),
    meterLabel: h("span"), meterValue: h("span.meter-value"), meterFill: h("i"),
    why: h("p.mission-why", { hidden: true }),
  };
  view.meter = h("div.meter", { role: "progressbar", "aria-label": "Goal progress" }, view.meterFill);
  view.chip.append(h("i.live-dot"), view.chipIcon, view.chipText);
  root.replaceChildren(
    h("div.eyebrow", {}, view.chip, view.persona, view.learning, view.ladder, view.tick),
    view.goal, view.meta,
    h("div.meter-block", {}, h("div.meter-label", {}, view.meterLabel, view.meterValue), view.meter),
    view.why,
  );
  let lastText = null;

  return function update(s, started) {
    const [label, ic] = STATE_LABEL[s.state] || [s.state || "idle", "dot"];
    const paused = !!s.paused;
    view.chip.classList.toggle("live", !paused && (s.state === "playing" || s.state === "picking" || s.state === "routing"));
    view.chip.classList.toggle("paused", paused);
    view.chipIcon.replaceChildren(icon(paused ? "pause" : ic));
    view.chipText.textContent = paused ? "Paused" : label;

    const persona = s.persona;
    view.persona.hidden = !persona;
    view.persona.textContent = persona ? `persona: ${persona.id}` : "";
    view.persona.title = persona && persona.style ? String(persona.style) : "";

    const learning = s.learning;
    view.learning.hidden = !learning;
    // textContent, never innerHTML: the skill name comes from a model.
    view.learning.textContent = learning ? `Learning ${learning.skill}` : "";
    view.learning.title = learning
      ? `attempt ${(learning.attempt ?? 0) + 1}${learning.seed != null ? `, practice seed ${learning.seed}` : ""}`
      : "";

    const done = ((s.ladder || {}).done || []).length;
    const rungs = (s.rungs || []).length;
    view.ladder.hidden = !rungs;
    view.ladder.textContent = rungs ? `ladder ${done}/${rungs}` : "";
    view.tick.textContent = s.connected === false ? "body disconnected" : s.tick === null || s.tick === undefined ? "offline" : `tick ${num(s.tick)}`;

    const goal = s.goal;
    const text = goal ? goal.text : s.paused ? "Paused" : s.state === "idle" || !s.state ? "Waiting for the bot" : "Choosing the next goal";
    const next = sentence(text);
    if (next !== lastText) {
      lastText = next;
      view.goal.classList.add("swap");
      setTimeout(() => { view.goal.textContent = next; view.goal.classList.remove("swap"); }, view.goal.textContent ? 160 : 0);
    }
    view.goal.classList.toggle("idle", !goal);

    const elapsed = goal ? (started ? Date.now() / 1000 - started : goal.ticks != null ? goal.ticks / TICKS_PER_SECOND : null) : null;
    view.meta.replaceChildren(
      ...(goal
        ? [h("code", { text: goal.condition }), h("span.sep", { text: "via" }), h("b", { text: goal.skill || "no skill" }),
          goal.origin === "user" ? chip("your task", "you") : null,
          elapsed !== null ? h("span.elapsed", {}, icon("clock"), span(elapsed) + (started ? "" : " (est.)")) : null]
        : [h("span.muted", { text: s.connected === false ? "No body is connected yet." : "The next goal appears here as soon as the bot picks one." })]).filter(Boolean),
    );

    const progress = goal ? countProgress(goal.condition, (s.facts || {}).inventory) : null;
    view.meter.classList.toggle("indeterminate", !!goal && !progress);
    view.meter.classList.toggle("idle", !goal);
    if (progress) {
      view.meterFill.style.width = `${progress.fraction * 100}%`;
      view.meterLabel.textContent = "Progress";
      view.meterValue.textContent = `${progress.have} / ${progress.need} ${words(progress.item)}`;
      view.meter.setAttribute("aria-valuenow", String(Math.round(progress.fraction * 100)));
    } else {
      view.meterFill.style.width = "";
      view.meterLabel.textContent = goal ? "Progress" : "";
      view.meterValue.textContent = goal ? "no countable target, working" : "";
      view.meter.removeAttribute("aria-valuenow");
    }
    const why = goal && goal.rationale ? String(goal.rationale) : "";
    view.why.textContent = why ? `Why: ${why}` : "";
    view.why.hidden = !why;
  };
}

// -- the thought --------------------------------------------------------- //
export function createThought(root) {
  const quote = h("blockquote.thought-text");
  const when = h("span.thought-when");
  const label = h("span.thought-label", { text: "Thinking" });
  root.replaceChildren(h("div.thought-head", {}, icon("brain"), label, when), quote);
  let at = null;
  const refreshWhen = () => { when.textContent = at ? relative(at) : ""; };
  setInterval(refreshWhen, 5000);

  return function update(thought, ts) {
    const text = thought ? String(thought).trim() : "";
    root.classList.toggle("empty", !text);
    if (!text) {
      quote.textContent = "Nothing on its mind yet. Its next plan shows up here, in its own words.";
      at = null;
    } else if (quote.textContent !== text) {
      quote.textContent = text;
      quote.classList.remove("fresh");
      void quote.offsetWidth; // restart the reveal
      quote.classList.add("fresh");
    }
    if (text) at = ts || at;
    refreshWhen();
  };
}

// -- the plan queue ------------------------------------------------------- //
const STEP_ICON = { done: "check", playing: "play", queued: "dot", skipped: "cross", failed: "cross", interrupted: "stop" };
const STEP_LABEL = { done: "done", playing: "playing", queued: "queued", skipped: "skipped", failed: "failed", interrupted: "stopped" };

export function createPlanStrip(root) {
  const list = h("ol.plan-strip");
  root.replaceChildren(h("h2", {}, "Plan", h("span.muted", { id: "plan-count" })), list);
  const count = root.querySelector("#plan-count");

  return function update(plan, queuedFromState) {
    let steps = plan ? plan.steps : [];
    if (!steps.length && queuedFromState && queuedFromState.length) {
      steps = queuedFromState.map((goal, index) => ({ index, text: goal.text, condition: goal.condition, skill: goal.skill, status: "queued", reason: null }));
    }
    root.classList.toggle("empty", !steps.length);
    if (!steps.length) {
      list.replaceChildren(h("li.plan-empty", { text: "No plan yet. The planner queues up to five goals at a time; they line up here." }));
      count.textContent = "";
      return;
    }
    const finished = steps.filter((step) => ["done", "failed", "skipped", "interrupted"].includes(step.status)).length;
    count.textContent = `${finished} of ${steps.length} settled`;
    list.replaceChildren(...steps.map((step) =>
      h("li.step." + step.status, { title: step.reason ? `${step.text}: skipped, ${words(step.reason)}` : step.text },
        h("span.step-ico", {}, icon(STEP_ICON[step.status] || "dot")),
        h("div.step-body", {},
          h("span.step-text", { text: sentence(step.text) }),
          h("span.step-sub", { text: step.reason ? `skipped: ${words(step.reason)}` : (step.skill || STEP_LABEL[step.status]) })))));
  };
}

// -- vitals -------------------------------------------------------------- //
export function createVitals(root) {
  function bar(label) {
    const fill = h("i");
    const value = h("span", { text: "-" });
    const track = h("div.bar", {}, fill);
    return { node: h("div.vital", {}, h("div.bar-label", {}, h("span", { text: label }), value), track), fill, value, track };
  }
  const view = {
    health: bar("Health"), food: bar("Food"), air: bar("Air"),
    pos: h("b"), dim: h("b"), inv: h("b"), time: h("b"), age: h("b"),
  };
  root.replaceChildren(
    h("h2", {}, "Body"),
    h("div.vitals", {}, view.health.node, view.food.node, view.air.node),
    h("div.facts", {},
      h("span", {}, "Position ", view.pos), h("span", {}, "Dimension ", view.dim),
      h("span", {}, "Inventory ", view.inv), h("span", {}, "Time ", view.time),
      h("span", { title: "Age of the last body snapshot" }, "Facts ", view.age)),
  );

  function set(b, value, max, lowBelow) {
    const known = value !== null && value !== undefined;
    b.value.textContent = known ? `${Math.round(value)} / ${max}` : "-";
    b.fill.style.width = known ? `${Math.max(0, Math.min(100, (value / max) * 100))}%` : "0";
    b.track.classList.toggle("low", known && value <= lowBelow);
  }

  return function update(s) {
    const f = s.facts || {};
    set(view.health, f.health, 20, 6);
    set(view.food, f.food, 20, 6);
    set(view.air, f.air, 300, 60);
    view.pos.textContent = f.pos ? f.pos.map((v) => Math.round(v)).join(", ") : "-";
    view.dim.textContent = (f.dimension || "-").replace("minecraft:", "");
    view.inv.textContent = f.inventory ? `${Object.values(f.inventory).reduce((a, b) => a + b, 0)} items` : "-";
    view.time.textContent = f.is_night === undefined || f.is_night === null ? "-" : f.is_night ? "night" : "day";
    const age = typeof s.snapshot_at === "number" ? Math.max(0, Date.now() / 1000 - s.snapshot_at) : null;
    view.age.textContent = age === null ? "-" : `${Math.round(age)} s old`;
    view.age.classList.toggle("warn", age !== null && age > STALE_SECONDS);
  };
}

// -- surroundings --------------------------------------------------------- //
export function createSurroundings(root) {
  const entities = h("div.chips");
  const blocks = h("div.chips");
  const sounds = h("div.chips");
  const group = (label, node) => h("div.sg", {}, h("span.sg-label", { text: label }), node);
  const groups = { entities: group("Creatures", entities), blocks: group("Blocks", blocks), sounds: group("Heard", sounds) };
  root.replaceChildren(h("h2", {}, "Surroundings"), groups.entities, groups.blocks, groups.sounds);

  return function update(nearby) {
    // Absent on an older body: the card disappears instead of showing empty shells.
    root.hidden = !nearby;
    if (!nearby) return;
    const fill = (node, group, pairs, make) => {
      group.hidden = !pairs.length;
      node.replaceChildren(...pairs.map(make));
    };
    const counted = (obj) => Object.entries(obj || {}).sort((a, b) => b[1] - a[1]);
    fill(entities, groups.entities, counted(nearby.entities), ([name, n]) => h("span.nchip.creature", { text: `${words(name)} ×${n}` }));
    fill(blocks, groups.blocks, counted(nearby.blocks), ([name, n]) => h("span.nchip.block", { text: `${words(name)} ×${n}` }));
    fill(sounds, groups.sounds, (nearby.sounds || []).map((x) => [x]), ([name]) => h("span.nchip.sound", { text: words(String(name).replace(/\./g, " ")) }));
  };
}

// -- ladder (the Run page's yardstick) ----------------------------------- //
export function renderLadder(root, s) {
  const rungs = s.rungs || [];
  const done = new Set((s.ladder || {}).done || []);
  const next = (s.ladder || {}).next;
  if (root.dataset.built !== String(rungs.length)) {
    root.dataset.built = String(rungs.length);
    root.replaceChildren(
      h("h2", { id: "ladder-title" }, "Ladder", h("span.muted", { id: "ladder-count" })),
      h("div.rungs", { id: "rungs", role: "list" },
        rungs.map((r) => h("div.rung", { role: "listitem", "data-id": r.id }, h("div.node", {}, icon("check")), h("span", { text: words(r.id) })))),
    );
  }
  root.querySelector("#ladder-count").textContent = `${done.size} / ${rungs.length}`;
  for (const node of root.querySelector("#rungs").children) {
    const id = node.dataset.id;
    const rung = rungs.find((r) => r.id === id);
    node.classList.toggle("met", done.has(id));
    node.classList.toggle("next", id === next);
    node.classList.toggle("blocked", !done.has(id) && !!rung && rung.blocked);
    node.title = rung ? `${rung.condition}${rung.blocked ? " (blocked after failures)" : ""}` : "";
  }
}

// -- spend ---------------------------------------------------------------- //
export function renderSpend(root, s) {
  const providers = Object.entries(s.budget || {});
  const spent = providers.reduce((a, [, v]) => a + v.spent, 0);
  const limit = providers.reduce((a, [, v]) => a + v.limit, 0);
  const share = limit ? Math.min(1, spent / limit) : 0;
  const r = 52, c = 2 * Math.PI * r;
  const ring = document.createElementNS("http://www.w3.org/2000/svg", "svg");
  ring.setAttribute("class", "ring");
  ring.setAttribute("viewBox", "0 0 130 130");
  ring.innerHTML = `<circle class="track" cx="65" cy="65" r="${r}"/><circle class="value" cx="65" cy="65" r="${r}" stroke-dasharray="${c}" stroke-dashoffset="${c}"/>`;
  const prior = root.querySelector(".ring .value");
  root.replaceChildren(
    h("h2", {}, "Spend", icon("coin")),
    h("div.ring-wrap", {}, ring,
      h("div", {}, h("div.big-num", { text: money(spent) }),
        h("div.small", { text: `of ${money(limit)} budget` }),
        h("div.small", { text: `${money(s.spent_last_hour)} in the last hour` }))),
    h("div.prov", {}, providers.map(([name, v]) => h("div", {}, h("span", { text: name }), h("span", { text: `${money(v.spent)} / ${money(v.limit)}` })))),
    h("div.prov", {}, h("div", {}, h("span", { text: "Spend guard" }),
      h("span", { text: s.governor ? (s.governor.code_only ? "code-only" : "normal") + ` (breaker ${s.governor.breaker})` : "-" }))),
  );
  const value = root.querySelector(".ring .value");
  value.style.strokeDashoffset = prior ? prior.style.strokeDashoffset : c;
  requestAnimationFrame(() => requestAnimationFrame(() => { value.style.strokeDashoffset = c * (1 - share); }));
}

export { svg };
