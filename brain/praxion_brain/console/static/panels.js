// The bento cards: hero, spend, ladder, metrics and skills.
import { h, icon, svg, el, money, num, sentence, sparkPath, STALE_SECONDS } from "./util.js";

const STATE_LABEL = {
  idle: ["Idle", "dot"], picking: ["Picking a goal", "brain"], routing: ["Routing", "route"],
  playing: ["Playing", "play"], starting: ["Starting", "dot"],
};

// -- hero --------------------------------------------------------------- //
let hero = null;

function bar(label) {
  const fill = h("i");
  const value = h("span", { text: "-" });
  const track = h("div.bar", {}, fill);
  return { node: h("div", {}, h("div.bar-label", {}, h("span", { text: label }), value), track), fill, value, track };
}

function buildHero() {
  const root = el("hero");
  hero = {
    chipIcon: h("span"), chipText: h("span"), chip: h("span.chip"),
    learning: h("span.chip.learning", { hidden: true }),
    tick: h("span.chip", { title: "Body tick" }), wake: h("span.muted"),
    mission: h("h1.mission"), sub: h("p.mission-sub"), why: h("p.mission-why", { hidden: true }), progress: h("i"),
    health: bar("Health"), food: bar("Food"), air: bar("Air"),
    pos: h("b"), dim: h("b"), inv: h("b"), skill: h("b"), time: h("b"), age: h("b"),
  };
  hero.chip.append(h("i.live-dot"), hero.chipIcon, hero.chipText);
  root.replaceChildren(
    h("div.eyebrow", {}, hero.chip, hero.learning, hero.tick, hero.wake),
    hero.mission, hero.sub, hero.why,
    h("div.progress", {}, hero.progress),
    h("div.vitals", {}, hero.health.node, hero.food.node, hero.air.node),
    h("div.facts", {},
      h("span", {}, "Position ", hero.pos), h("span", {}, "Dimension ", hero.dim),
      h("span", {}, "Skill ", hero.skill), h("span", {}, "Inventory ", hero.inv), h("span", {}, "Time ", hero.time),
      h("span", { title: "Age of the last body snapshot" }, "Facts ", hero.age)),
  );
}

function setBar(b, value, max, lowBelow) {
  const known = value !== null && value !== undefined;
  b.value.textContent = known ? `${Math.round(value)} / ${max}` : "-";
  b.fill.style.width = known ? `${Math.max(0, Math.min(100, (value / max) * 100))}%` : "0";
  b.track.classList.toggle("low", known && value <= lowBelow);
}

export function renderHero(s) {
  if (!hero) buildHero();
  const [label, ic] = STATE_LABEL[s.state] || [s.state, "dot"];
  const paused = s.paused;
  hero.chip.classList.toggle("live", !paused && (s.state === "playing" || s.state === "picking"));
  hero.chip.classList.toggle("paused", paused);
  hero.chipIcon.replaceChildren(icon(paused ? "pause" : ic));
  hero.chipText.textContent = paused ? "Paused" : label;
  const learning = s.learning;
  hero.learning.hidden = !learning;
  // textContent, never innerHTML: the skill name comes from a model.
  hero.learning.textContent = learning ? `Learning: ${learning.skill}` : "";
  hero.learning.title = learning
    ? `attempt ${(learning.attempt ?? 0) + 1}${learning.seed != null ? `, practice seed ${learning.seed}` : ""}`
    : "";
  hero.tick.textContent = s.tick === null || s.tick === undefined ? "offline" : `tick ${num(s.tick)}`;
  hero.wake.textContent = s.connected === false ? "body disconnected" : s.wake ? `waking: ${s.wake}` : "";

  const goal = s.goal;
  const text = goal ? goal.text : s.proposal && s.state !== "idle" ? s.proposal.text : s.paused ? "Paused" : "Waiting for the next goal";
  const next = sentence(text);
  if (hero.mission.textContent !== next) {
    hero.mission.classList.add("swap");
    setTimeout(() => { hero.mission.textContent = next; hero.mission.classList.remove("swap"); }, 180);
  }
  hero.sub.replaceChildren(
    ...(goal
      ? [h("code", { text: goal.condition }), ` via ${goal.skill}`, goal.origin === "user" ? " (your goal)" : "", `, ${num(goal.ticks)} ticks in`]
      : s.proposal ? ["Last: ", h("code", { text: s.proposal.condition })] : []),
  );
  const why = goal && goal.rationale ? String(goal.rationale) : "";
  hero.why.textContent = why;
  hero.why.hidden = !why;
  hero.progress.style.width = goal && goal.ticks != null ? `${Math.min(100, (goal.ticks / 6000) * 100)}%` : "0";

  const f = s.facts || {};
  setBar(hero.health, f.health, 20, 6);
  setBar(hero.food, f.food, 20, 6);
  setBar(hero.air, f.air, 300, 60);
  hero.pos.textContent = f.pos ? f.pos.join(", ") : "-";
  hero.dim.textContent = (f.dimension || "-").replace("minecraft:", "");
  hero.skill.textContent = goal ? goal.skill : "-";
  hero.inv.textContent = f.inventory ? `${Object.values(f.inventory).reduce((a, b) => a + b, 0)} items` : "-";
  hero.time.textContent = f.is_night === undefined || f.is_night === null ? "-" : f.is_night ? "night" : "day";
  const age = typeof s.snapshot_at === "number" ? Math.max(0, Date.now() / 1000 - s.snapshot_at) : null;
  hero.age.textContent = age === null ? "-" : `${Math.round(age)} s old`;
  hero.age.classList.toggle("warn", age !== null && age > STALE_SECONDS);
}

// -- spend -------------------------------------------------------------- //
export function renderSpend(s) {
  const providers = Object.entries(s.budget || {});
  const spent = providers.reduce((a, [, v]) => a + v.spent, 0);
  const limit = providers.reduce((a, [, v]) => a + v.limit, 0);
  const share = limit ? Math.min(1, spent / limit) : 0;
  const r = 52, c = 2 * Math.PI * r;
  const ring = document.createElementNS("http://www.w3.org/2000/svg", "svg");
  ring.setAttribute("class", "ring");
  ring.setAttribute("viewBox", "0 0 130 130");
  ring.innerHTML = `<circle class="track" cx="65" cy="65" r="${r}"/><circle class="value" cx="65" cy="65" r="${r}" stroke-dasharray="${c}" stroke-dashoffset="${c}"/>`;
  const prior = el("spend").querySelector(".ring .value");
  el("spend").replaceChildren(
    h("h2", {}, "Spend", icon("coin")),
    h("div.ring-wrap", {}, ring,
      h("div", {}, h("div.big-num", { text: money(spent) }),
        h("div.small", { text: `of ${money(limit)} budget` }),
        h("div.small", { text: `${money(s.spent_last_hour)} in the last hour` }))),
    h("div.prov", {}, providers.map(([name, v]) => h("div", {}, h("span", { text: name }), h("span", { text: `${money(v.spent)} / ${money(v.limit)}` })))),
    h("div.prov", {}, h("div", {}, h("span", { text: "Governor" }),
      h("span", { text: s.governor ? (s.governor.code_only ? "code-only" : "normal") + ` (breaker ${s.governor.breaker})` : "-" }))),
  );
  const value = el("spend").querySelector(".ring .value");
  value.style.strokeDashoffset = prior ? prior.style.strokeDashoffset : c;
  requestAnimationFrame(() => requestAnimationFrame(() => { value.style.strokeDashoffset = c * (1 - share); }));
}

// -- ladder ------------------------------------------------------------- //
export function renderLadder(s) {
  const rungs = s.rungs || [];
  const done = new Set((s.ladder || {}).done || []);
  const next = (s.ladder || {}).next;
  const root = el("ladder");
  if (root.dataset.built !== String(rungs.length)) {
    root.dataset.built = String(rungs.length);
    root.replaceChildren(h("h2", { id: "ladder-title" }, "Ladder", h("span.muted", { id: "ladder-count" })), h("div.rungs", { id: "rungs", role: "list" },
      rungs.map((r) => h("div.rung", { role: "listitem", "data-id": r.id }, h("div.node", {}, icon("check")), h("span", { text: r.id.replace(/_/g, " ") })))));
  }
  el("ladder-count").textContent = `${done.size} / ${rungs.length}`;
  for (const node of el("rungs").children) {
    const id = node.dataset.id;
    const rung = rungs.find((r) => r.id === id);
    node.classList.toggle("met", done.has(id));
    node.classList.toggle("next", id === next);
    node.classList.toggle("blocked", !done.has(id) && !!rung && rung.blocked);
    node.title = rung ? `${rung.condition}${rung.blocked ? " (blocked after failures)" : ""}` : "";
  }
}

// -- metrics ------------------------------------------------------------ //
function chart(title, values, hover) {
  const w = 300, hgt = 70;
  const line = sparkPath(values, w, hgt);
  const area = values.length ? `${line} L${w - 4} ${hgt - 4} L4 ${hgt - 4} Z` : "";
  const node = svg(`<path class="area" d="${area}"/><path class="line" d="${line}"/>`);
  node.setAttribute("viewBox", `0 0 ${w} ${hgt}`);
  node.setAttribute("preserveAspectRatio", "none");
  node.setAttribute("stroke-width", "");
  node.setAttribute("fill", "none");
  return h("div.chart", { title: hover }, h("h3", { text: title }), values.length ? node : h("div.small", { text: "no data yet" }));
}

export function renderMetrics(m) {
  const cost = m.cost_per_hour.map((b) => b.cost);
  const rungs = m.rungs.map((r) => r.rungs);
  const skills = Object.entries(m.goals_by_skill).sort((a, b) => total(b[1]) - total(a[1])).slice(0, 8);
  el("metrics").replaceChildren(
    h("h2", {}, "Metrics", icon("spark")),
    h("div.stat-row", {},
      h("div.stat", {}, h("b", { text: num(m.totals.done) }), h("span.small", { text: "goals done" })),
      h("div.stat", {}, h("b", { text: num(m.totals.failed) }), h("span.small", { text: "failed" })),
      h("div.stat", {}, h("b", { text: m.ticks.median === null ? "-" : num(Math.round(m.ticks.median)) }), h("span.small", { text: "median ticks" }))),
    chart("Cost per hour", cost, `last ${cost.length} h, peak ${money(Math.max(0, ...cost))}`),
    chart("Rungs met over goals", rungs, `${rungs.length} goals`),
    h("div.skill-bars", {}, skills.map(([name, c]) => {
      const t = total(c), other = t - (c.done || 0) - (c.failed || 0);
      return h("div.sbar", { title: JSON.stringify(c) }, h("span.name", { text: name }),
        h("div.stack", {}, h("i.seg-done", { style: `width:${((c.done || 0) / t) * 100}%` }), h("i.seg-failed", { style: `width:${((c.failed || 0) / t) * 100}%` }), h("i.seg-other", { style: `width:${(other / t) * 100}%` })),
        h("span.small", { text: t }));
    })),
  );
}
const total = (c) => Object.values(c).reduce((a, b) => a + b, 0);

// -- skills ------------------------------------------------------------- //
export function renderSkills(data, open) {
  const pill = (cls, label, onclick, title) => h("button.pill." + cls, { type: "button", onclick, title }, label);
  el("skills").replaceChildren(
    h("h2", {}, "Skills", icon("box")),
    h("div.skill-cols", {},
      h("div", {}, h("h3", { text: `Core (${data.core.length})` }), h("div.pills", {}, data.core.map((k) => pill("core", k.name, () => open("skill", k), k.doc)))),
      h("div", {}, h("h3", { text: `Learned (${data.learned.length})` }), h("div.pills", {}, data.learned.length ? data.learned.map((k) => pill("learned", [icon("spark"), k.name], () => open("skill", k), k.doc)) : h("span.small", { text: "nothing admitted yet" }))),
      h("div", {}, h("h3", { text: `Candidates (${data.candidates.length})` }), h("div.pills", {}, data.candidates.length ? data.candidates.map((k) => pill("cand" + (k.status === "admitted" ? "" : " rejected"), `${k.skill} - ${k.status}`, () => open("candidate", k), k.reason || "")) : h("span.small", { text: "none" })))),
  );
}
