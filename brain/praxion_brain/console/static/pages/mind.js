// The Mind page: every model exchange as a readable card, with latency and cost on top.
import { h, icon, el, money, num, relative, epoch, words, recall, remember } from "../util.js";
import { getJSON } from "../api.js";
import { tint } from "../inspector.js";
import { onEvidence } from "../store.js";

const FAMILIES = [
  ["all", "All", "spark"],
  ["plan", "Planner", "brain"],
  ["router", "Routing", "route"],
  ["command_map", "Commands", "user"],
  ["writer", "Skill writing", "pen"],
  ["proposal", "Proposals", "flag"],
];
const TITLE = { plan: "Planner", router: "Jev routing", command_map: "Command mapping", writer: "Skill writer", proposal: "Goal proposal" };
const ICON = { plan: "brain", router: "route", command_map: "user", writer: "pen", proposal: "flag" };
const REFRESH_MS = 8000;
const RESPONSE_KINDS = new Set(["plan_response", "router_response", "command_map_response", "writer_response", "proposal_response"]);

const seconds = (v) => (v === null || v === undefined ? "-" : v < 10 ? v.toFixed(1) + " s" : Math.round(v) + " s");

function stat(label, value, sub) {
  return h("div.stat", {}, h("span.stat-label", { text: label }), h("b", { text: value }), sub ? h("span.small", { text: sub }) : null);
}

function scoreRows(scores, decisionSkill) {
  const entries = Object.entries(scores || {}).sort((a, b) => b[1] - a[1]).slice(0, 6);
  if (!entries.length) return null;
  return h("div.scores", {}, entries.map(([name, p]) =>
    h("div.score" + (name === decisionSkill ? ".top" : ""), {},
      h("span.name", { text: name }), h("div.bar", {}, h("i", { style: `width:${Math.round(p * 100)}%` })), h("span.small", { text: Math.round(p * 100) + "%" }))));
}

function body(row) {
  const parsed = row.parsed && typeof row.parsed === "object" ? row.parsed : null;
  if (row.error) return h("p.xc-error", { text: String(row.error) });
  switch (row.kind) {
    case "plan": {
      if (!parsed) return h("p.muted", { text: "No parsed plan in this response." });
      return h("div.xc-plan", {},
        h("blockquote.xc-thought", { text: parsed.thought || "" }),
        h("ol.xc-goals", {}, (parsed.goals || []).map((g) => h("li", {},
          h("span.g-text", { text: g.text }),
          h("code", { text: g.condition || "(skill-verified)" }),
          g.skill ? h("span.tag", { text: g.skill }) : null,
          g.rationale ? h("em", { text: g.rationale }) : null))),
        row.briefing ? h("details.xc-brief", {}, h("summary", { text: "Briefing it was given" }), h("pre.code", { text: row.briefing })) : null);
    }
    case "router": {
      const verb = row.decision === "play" ? `plays ${row.skill}` : row.decision ? `${row.decision}s a new skill` : "no decision recorded";
      return h("div.xc-route", {},
        row.goal ? h("p.xc-in", {}, h("span.muted", { text: "Goal " }), row.goal) : null,
        h("p.xc-decision", {}, icon("route"), h("b", { text: verb }),
          row.source ? h("span.tag", { text: words(row.source) }) : null,
          row.hint ? h("span.tag", { text: "hint " + row.hint }) : null),
        scoreRows(row.scores, row.skill));
    }
    case "command_map": {
      const p = parsed || {};
      return h("div.xc-cmd", {},
        row.text ? h("p.xc-in", {}, h("span.muted", { text: "Operator said " }), h("q", { text: row.text })) : null,
        h("p.xc-decision", {}, icon("arrow"),
          p.skill ? h("b", { text: `skill ${p.skill}` }) : p.condition ? h("code", { text: p.condition }) : h("b", { text: "no goal" }),
          p.risk ? h("span.tag", { text: "risk " + p.risk }) : null),
        p.unsupported ? h("p.muted", { text: p.unsupported }) : null);
    }
    case "writer": {
      const p = parsed || {};
      const pre = h("pre.code");
      pre.append(tint(String(p.skill_source || "")));
      return h("div.xc-writer", {}, p.rationale ? h("p", { text: p.rationale }) : null, p.skill_source ? h("details.xc-brief", {}, h("summary", { text: "Skill as written" }), pre) : null);
    }
    default: {
      const p = parsed || {};
      return h("div.xc-plan", {}, h("p.g-text", { text: p.text || "" }), p.condition ? h("code", { text: p.condition }) : null, p.rationale ? h("em", { text: p.rationale }) : null);
    }
  }
}

function card(row, open) {
  const at = epoch(row.timestamp);
  return h("article.xc.fam-" + row.kind, {},
    h("header", {},
      h("span.fi-ico", {}, icon(ICON[row.kind] || "dot")),
      h("div.xc-title", {}, h("h3", { text: TITLE[row.kind] || row.kind }), h("span.muted", { text: [row.provider, row.model].filter(Boolean).join(" / ") })),
      h("div.xc-metrics", {},
        h("span.metric", { title: "Latency", text: seconds(row.latency_s) }),
        h("span.metric", { title: "Cost", text: money(row.cost) }),
        h("span.metric.dim", { title: "Tokens in / out", text: `${num(row.input_tokens)} / ${num(row.output_tokens)} tok` })),
      h("time.rel", { text: relative(at), "data-ts": at || "" })),
    body(row),
    h("footer", {}, h("button.inspect", { type: "button", onclick: () => open("exchange", row.n) }, icon("eye"), "Inspect full exchange"),
      row.invalid_count ? h("span.tag.warn", { text: `${row.invalid_count} invalid before this` }) : null,
      row.after_death ? h("span.tag.warn", { text: "after a death" }) : null));
}

export function mount(ctx) {
  const root = el("page-mind");
  let family = recall("mind.family", "all");
  let data = { items: [], summary: { all: { count: 0 }, by_kind: {} } };
  let visible = false;
  let loaded = false;
  let timer = null;
  let timer2 = null;
  let lastText = "";

  const summary = h("div.stat-row.wide-stats");
  const perKind = h("div.kind-table");
  const filters = h("div.feed-filters", { role: "group", "aria-label": "Filter exchanges" });
  const list = h("div.xc-list");
  const note = h("p.page-note", { text: "" });
  root.replaceChildren(
    h("div.page-head", {}, h("h1", { text: "Mind" }), h("p", { text: "Every exchange with a model, newest first: what it was asked, what it answered, how long it took and what it cost." })),
    summary, perKind, filters, note, list);

  function render() {
    const all = data.items;
    const count = (id) => (id === "all" ? all.length : all.filter((r) => r.kind === id).length);
    const rows = family === "all" ? all : all.filter((r) => r.kind === family);
    // The headline numbers follow the filter, so "planner latency" is one click away.
    const sum = family === "all" ? data.summary.all : data.summary.by_kind[family] || { count: 0 };
    summary.replaceChildren(
      stat("Exchanges", num(sum.count)),
      stat("Spend", money(sum.cost || 0), "this list"),
      stat("Median latency", seconds(sum.latency_p50), `p95 ${seconds(sum.latency_p95)}`),
      stat("Slowest", seconds(sum.latency_max)),
      stat("Tokens", num((sum.input_tokens || 0) + (sum.output_tokens || 0)), `${num(sum.input_tokens || 0)} in, ${num(sum.output_tokens || 0)} out`));
    perKind.replaceChildren(...Object.entries(data.summary.by_kind).map(([kind, b]) =>
      h("div.kt-row", {}, h("span.kt-name", {}, icon(ICON[kind] || "dot"), TITLE[kind] || kind), h("span", { text: `${b.count} ${b.count === 1 ? "call" : "calls"}` }),
        h("span", { text: `median ${seconds(b.latency_p50)}` }), h("span", { text: money(b.cost) }))));
    filters.replaceChildren(...FAMILIES.map(([id, label]) =>
      h("button.fchip", { type: "button", "aria-pressed": String(id === family), onclick: () => { family = id; remember("mind.family", id); render(); } },
        label, h("span.count", { text: String(count(id)) }))));
    note.hidden = rows.length > 0;
    note.textContent = loaded
      ? "No exchanges of this kind in the evidence yet. They appear here the moment the bot asks a model something."
      : "Loading...";
    list.replaceChildren(...rows.map((row) => card(row, ctx.open)));
  }

  async function load() {
    try {
      const next = await getJSON("/api/exchanges?limit=120");
      const text = JSON.stringify(next);
      loaded = true;
      if (text === lastText) return; // unchanged: keep the viewer's open details
      lastText = text;
      data = next;
      render();
    } catch {
      note.hidden = false;
      note.textContent = "Could not read the evidence log.";
    }
  }

  let debounce = null;
  onEvidence((entry) => {
    if (!visible || !RESPONSE_KINDS.has(entry.kind)) return;
    clearTimeout(debounce);
    debounce = setTimeout(load, 600);
  });

  render();
  return {
    show() {
      visible = true;
      load();
      timer = setInterval(() => { for (const t of list.querySelectorAll("time.rel[data-ts]")) if (t.dataset.ts) t.textContent = relative(Number(t.dataset.ts)); }, 5000);
      timer2 = setInterval(load, REFRESH_MS);
    },
    hide() { visible = false; clearInterval(timer); clearInterval(timer2); },
  };
}
