// The live feed: goal cards, plans, and the conversation, newest at the bottom.
//
// Behaviour the presentation depends on:
//  - follow: the list sticks to the newest line unless the viewer scrolled up;
//    a "jump to live" button (with the count of lines missed) brings it back.
//  - no layout jump: lines only ever append below; a card that finishes keeps
//    its height (the outcome is one reserved line), and the browser's scroll
//    anchoring holds the view still when something above changes.
//  - capped DOM: the model keeps MAX_ITEMS items and the nodes follow it.
import { h, icon, relative, span, words, recall, remember } from "./util.js";
import { createTimeline, outcome, CATEGORIES } from "./cards.js";

const FILTERS = [
  ["all", "All"],
  ["decisions", "Decisions"],
  ["goals", "Goals"],
  ["talk", "Talk"],
  ["problems", "Problems"],
];
const FOLLOW_SLACK_PX = 48;
const TICK_MS = 5000;

const ORIGIN = {
  plan: ["Planned", ""],
  you: ["Your task", "you"],
  survival: ["Survival", "reflex"],
  fallback: ["Fallback", "warn"],
  direct: ["", ""],
};

function scoreBars(route) {
  const entries = Object.entries(route.scores || {}).sort((a, b) => b[1] - a[1]).slice(0, 4);
  if (!entries.length) return null;
  return h("div.mini-scores", {}, entries.map(([name, p], i) =>
    h("div.mini-score" + (i === 0 ? ".top" : ""), {},
      h("span.name", { text: name }),
      h("div.bar", {}, h("i", { style: `width:${Math.round(p * 100)}%` })),
      h("span.pct", { text: Math.round(p * 100) + "%" }))));
}

export function createFeed(root, { open }) {
  const timeline = createTimeline();
  const nodes = new Map();
  const expanded = new Set();
  const listeners = new Set();
  let filter = recall("feed.filter", "all");
  if (!FILTERS.some(([id]) => id === filter)) filter = "all";
  let pinned = true;
  let missed = 0;
  let frame = 0;

  // -- shell ------------------------------------------------------------ //
  const chips = new Map();
  const bar = h("div.feed-filters", { role: "group", "aria-label": "Filter the feed" },
    FILTERS.map(([id, label]) => {
      const count = h("span.count", { text: "0" });
      const button = h("button.fchip", { type: "button", "aria-pressed": String(id === filter), onclick: () => setFilter(id) }, label, count);
      chips.set(id, { button, count });
      return button;
    }));
  const list = h("ol.feed-list");
  const empty = h("div.feed-empty", {},
    h("div.feed-empty-mark", {}, icon("brain")),
    h("p.feed-empty-title", { text: "Nothing to show yet" }),
    h("p", { text: "Every thought, goal and spoken line lands here, grouped by goal, as soon as the bot starts playing." }));
  const scroller = h("div.feed-scroll", { tabindex: "0", "aria-label": "Live feed lines" }, list, empty);
  const jumpCount = h("span.jump-count");
  const jump = h("button.jump", { type: "button", hidden: true, onclick: () => toLive(true) }, icon("arrow"), "Jump to live", jumpCount);
  root.replaceChildren(
    h("div.feed-head", {}, h("h2", {}, "Live feed", h("span.feed-live", { text: "following" })), bar),
    h("div.feed-body", {}, scroller, jump),
  );
  const followLabel = root.querySelector(".feed-live");

  // -- following -------------------------------------------------------- //
  function distance() {
    return scroller.scrollHeight - scroller.scrollTop - scroller.clientHeight;
  }

  function toLive(smooth) {
    scroller.scrollTo({ top: scroller.scrollHeight, behavior: smooth && !matchMedia("(prefers-reduced-motion: reduce)").matches ? "smooth" : "auto" });
    pinned = true;
    missed = 0;
    showFollow();
  }

  function showFollow() {
    jump.hidden = pinned;
    jumpCount.textContent = missed ? ` ${missed} new` : "";
    followLabel.textContent = pinned ? "following" : "paused";
    followLabel.classList.toggle("paused", !pinned);
  }

  scroller.addEventListener("scroll", () => {
    const near = distance() <= FOLLOW_SLACK_PX;
    if (near !== pinned) {
      pinned = near;
      if (pinned) missed = 0;
      showFollow();
    }
  }, { passive: true });

  // -- rendering -------------------------------------------------------- //
  function matches(item) {
    return filter === "all" || item.cats.includes(filter);
  }

  function timeNode(item) {
    const node = h("time.rel", { text: relative(item.ts) });
    if (item.ts) {
      node.dataset.ts = item.ts;
      node.setAttribute("datetime", new Date(item.ts * 1000).toISOString());
      node.title = new Date(item.ts * 1000).toLocaleTimeString("en-GB");
    }
    return node;
  }

  function toggle(item, detail, button) {
    const on = !expanded.has(item.id);
    if (on) expanded.add(item.id); else expanded.delete(item.id);
    detail.hidden = !on;
    button.setAttribute("aria-expanded", String(on));
    if (pinned) toLive(false);
  }

  function inspectButton(label, n) {
    return n ? h("button.inspect", { type: "button", onclick: () => open("exchange", n) }, icon("eye"), label) : null;
  }

  function goalCard(item) {
    const o = outcome(item);
    const [originLabel, originTone] = ORIGIN[item.origin] || ["", ""];
    const took = item.t0 && item.t1 ? item.t1 - item.t0 : null;
    const facts = [];
    if (item.status === "done") facts.push(took !== null ? `in ${span(took)}` : "");
    if (item.status === "failed" && took !== null) facts.push(`after ${span(took)}`);
    if (item.ticks !== null && item.ticks !== undefined) facts.push(`${item.ticks} ticks`);
    if (item.rungs !== null && item.rungs !== undefined) facts.push(`${item.rungs} rungs`);
    const line = h("p.fi-outcome", {});
    if (item.status === "running") {
      line.append("Playing for ", h("b.elapsed", { "data-t0": item.t0 || "", text: item.t0 ? span(Date.now() / 1000 - item.t0) : "..." }));
    } else {
      line.append(h("b", { text: o.label }), facts.filter(Boolean).length ? " " + facts.filter(Boolean).join(" · ") : "");
      if (o.detail) line.append(h("span.reason", { text: " — " + o.detail }));
    }
    const detail = h("div.fi-detail", { hidden: !expanded.has(item.id) },
      item.condition ? h("div.kv", {}, h("span", { text: "Condition" }), h("code", { text: item.condition })) : null,
      item.why ? h("div.kv", {}, h("span", { text: "Why" }), h("em", { text: item.why })) : null,
      item.route ? h("div.kv", {}, h("span", { text: "Route" }),
        h("div", {}, h("div", { text: `${words(item.route.decision)} ${item.route.skill ? item.route.skill : "a new skill"}${item.route.source ? ` (${words(item.route.source)})` : ""}${item.route.hint ? `, hint ${item.route.hint}` : ""}` }), scoreBars(item.route))) : null,
      item.detail ? h("div.kv", {}, h("span", { text: "Detail" }), h("span", { text: String(item.detail) })) : null,
      h("div.fi-actions", {}, inspectButton("Plan exchange", item.planN), inspectButton("Routing exchange", item.route && item.route.n)),
    );
    const button = h("button.fi-toggle", { type: "button", "aria-expanded": String(expanded.has(item.id)), "aria-label": "Toggle details", onclick: (ev) => { ev.stopPropagation(); toggle(item, detail, button); } }, icon("chevron"));
    const head = h("div.fi-head", { onclick: () => toggle(item, detail, button) },
      h("span.fi-ico", {}, icon(item.status === "done" ? "check" : item.status === "failed" ? "cross" : item.status === "interrupted" ? "stop" : "play")),
      h("div.fi-main", {},
        h("div.fi-kicker", {},
          originLabel ? h("span.tag" + (originTone ? "." + originTone : ""), { text: originLabel }) : null,
          item.stepNo ? h("span.tag", { text: `step ${item.stepNo}/${item.stepCount}` }) : null,
          h("span.skill", { text: item.skill || "" })),
        h("h3.fi-title", { text: item.text })),
      h("div.fi-side", {}, h("span.status", { text: o.label }), timeNode(item)),
      button);
    return h("article.card-body", {}, head, line, detail);
  }

  function planCard(item) {
    const detail = h("div.fi-detail", { hidden: !expanded.has(item.id) },
      item.steps.map((step) => h("div.step-row", {},
        h("span.dot." + step.status, { title: step.reason || step.status }),
        h("div", {},
          h("div.step-text", { text: step.text }),
          h("div.step-sub", {}, h("code", { text: step.condition || "(skill-verified)" }), step.skill ? h("span.skill", { text: step.skill }) : null,
            step.reason ? h("span.reason", { text: "skipped: " + words(step.reason) }) : null),
          step.rationale ? h("em.step-why", { text: step.rationale }) : null))),
      h("div.fi-actions", {}, inspectButton("Open exchange", item.exchangeN)));
    const button = h("button.fi-toggle", { type: "button", "aria-expanded": String(expanded.has(item.id)), "aria-label": "Toggle details", onclick: (ev) => { ev.stopPropagation(); toggle(item, detail, button); } }, icon("chevron"));
    const strip = h("div.step-strip", {}, item.steps.map((step) => h("span.dot." + step.status, { title: `${step.text} (${step.status}${step.reason ? ": " + words(step.reason) : ""})` })));
    const head = h("div.fi-head", { onclick: () => toggle(item, detail, button) },
      h("span.fi-ico", {}, icon("brain")),
      h("div.fi-main", {},
        h("div.fi-kicker", {}, h("span.tag", { text: item.tag }), h("span.tag", { text: `${item.steps.length} goal${item.steps.length === 1 ? "" : "s"}` }), strip),
        item.thought ? h("blockquote.fi-thought", { text: item.thought })
          : h("h3.fi-title", { text: item.steps[0] ? item.steps[0].text : "" })),
      h("div.fi-side", {}, timeNode(item)),
      button);
    return h("article.card-body", {}, head, detail);
  }

  function build(item) {
    let body;
    let cls = "fi " + item.type + (item.tone ? " tone-" + item.tone : "") + (item.minor ? " minor" : "");
    if (item.type === "goal") body = goalCard(item);
    else if (item.type === "plan") body = planCard(item);
    else if (item.type === "you") {
      body = h("div.bubble", {},
        h("div.bubble-meta", {}, icon("user"), h("span", { text: item.source === "chat" ? "You, in chat" : "You" }), timeNode(item)),
        h("p.bubble-text", { text: item.text }),
        item.result ? h("p.bubble-result", { text: item.result }) : null);
    } else if (item.type === "say") {
      body = h("div.bubble", {},
        h("div.bubble-meta", {}, icon("chat"), h("span", { text: "Praxion" }), item.topic ? h("span.tag", { text: words(item.topic) }) : null, timeNode(item)),
        h("p.bubble-text", { text: item.text }));
    } else {
      cls += " flavor-" + (item.flavor || "system");
      const exchange = item.exchange ? () => open("exchange", item.n) : null;
      body = h("div.marker-row" + (exchange ? ".click" : ""), exchange ? { role: "button", tabindex: "0", onclick: exchange, onkeydown: (k) => { if (k.key === "Enter" || k.key === " ") { k.preventDefault(); exchange(); } } } : {},
        h("span.fi-ico", {}, icon(item.icon || "dot")),
        h("div.fi-main", {}, h("div.marker-title", { text: item.title }), item.sub ? h("div.marker-sub", { text: item.sub }) : null),
        timeNode(item));
    }
    const node = h("li", { class: cls, "data-id": item.id, "data-cats": item.cats.join(" "), "data-status": item.status || null }, body);
    node.hidden = !matches(item);
    return node;
  }

  // -- applying changes ------------------------------------------------- //
  function apply(changes, animate) {
    let fresh = 0;
    // One goal can change several times in a batch (sent, then finished): draw
    // its final state once.
    const merged = new Map();
    for (const { item, isNew } of changes) merged.set(item, merged.get(item) || isNew);
    for (const [item, isNew] of merged) {
      const node = build(item);
      if (!animate || !isNew) node.classList.add("still");
      const existing = nodes.get(item.id);
      if (existing) existing.replaceWith(node);
      else list.append(node);
      nodes.set(item.id, node);
      if (isNew && !node.hidden) fresh += 1;
    }
    if (nodes.size > timeline.items.length) {
      const live = new Set(timeline.items.map((i) => i.id));
      for (const [id, node] of nodes) if (!live.has(id)) { node.remove(); nodes.delete(id); }
    }
    if (animate) {
      if (pinned) toLive(false);
      else if (fresh) { missed += fresh; showFollow(); }
    }
    schedule();
  }

  function schedule() {
    if (frame) return;
    frame = requestAnimationFrame(() => {
      frame = 0;
      refreshCounts();
      for (const fn of listeners) fn(timeline);
    });
  }

  function refreshCounts() {
    const counts = { all: timeline.items.length };
    for (const category of CATEGORIES) counts[category] = timeline.items.filter((i) => i.cats.includes(category)).length;
    for (const [id, { count }] of chips) count.textContent = String(counts[id] ?? 0);
    const visible = timeline.items.some(matches);
    empty.hidden = visible;
    empty.querySelector(".feed-empty-title").textContent = timeline.items.length ? "No lines in this filter yet" : "Nothing to show yet";
    empty.querySelector("p:last-child").textContent = timeline.items.length
      ? "Switch to All to see everything the bot did."
      : "Every thought, goal and spoken line lands here, grouped by goal, as soon as the bot starts playing.";
  }

  function setFilter(id) {
    filter = id;
    remember("feed.filter", id);
    for (const [key, { button }] of chips) button.setAttribute("aria-pressed", String(key === id));
    for (const item of timeline.items) {
      const node = nodes.get(item.id);
      if (node) node.hidden = !matches(item);
    }
    refreshCounts();
    toLive(false);
  }

  // -- clock ------------------------------------------------------------- //
  function tick() {
    const now = Date.now() / 1000;
    for (const node of list.querySelectorAll("time.rel[data-ts]")) node.textContent = relative(Number(node.dataset.ts), now);
    for (const node of list.querySelectorAll("b.elapsed[data-t0]")) if (node.dataset.t0) node.textContent = span(now - Number(node.dataset.t0));
  }
  setInterval(tick, TICK_MS);

  return {
    timeline,
    add(entry) { apply(timeline.ingest(entry), true); },
    load(lines) {
      const changes = [];
      for (const entry of lines) changes.push(...timeline.ingest(entry));
      apply(changes, false);
      toLive(false);
    },
    onChange(fn) { listeners.add(fn); fn(timeline); return () => listeners.delete(fn); },
    refresh: tick,
  };
}
