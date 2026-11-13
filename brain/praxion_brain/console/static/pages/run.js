// The Run page: the ladder as the yardstick, the goals timeline, and run totals.
import { h, el, num, money, span, words, recall } from "../util.js";
import { getJSON } from "../api.js";
import { store, onState } from "../store.js";
import { renderLadder, renderSpend } from "../panels.js";
import { goalsTimeline, rungsChart, spendChart, tooltipFor } from "../charts.js";

const REFRESH_MS = 10000;

function stat(label, value, sub) {
  return h("div.stat", {}, h("span.stat-label", { text: label }), h("b", { text: value }), sub ? h("span.small", { text: sub }) : null);
}

const LEGEND = [["done", "Done"], ["failed", "Failed"], ["interrupted", "Interrupted"], ["running", "Running"]];

/** Goals per skill and outcome, counted from the goals the timeline shows. */
function countBySkill(goals) {
  const out = {};
  for (const g of goals || []) {
    const row = (out[g.skill || "(none)"] ||= {});
    row[g.status] = (row[g.status] || 0) + 1;
  }
  return out;
}

function skillBars(bySkill) {
  const total = (c) => Object.values(c).reduce((a, b) => a + b, 0);
  const rows = Object.entries(bySkill).sort((a, b) => total(b[1]) - total(a[1])).slice(0, 10);
  if (!rows.length) return h("p.muted.chart-empty", { text: "No goals recorded for this world yet." });
  const most = total(rows[0][1]);
  return h("div.skill-bars", {}, rows.map(([name, c]) => {
    const t = total(c);
    const other = t - (c.done || 0) - (c.failed || 0);
    const pct = (n) => `${(n / most) * 100}%`;
    return h("div.sbar", { title: JSON.stringify(c) },
      h("span.name", { text: name }),
      h("div.stack", {}, h("i.seg-done", { style: `width:${pct(c.done || 0)}` }), h("i.seg-failed", { style: `width:${pct(c.failed || 0)}` }), h("i.seg-other", { style: `width:${pct(other)}` })),
      h("span.small", { text: `${c.done || 0} / ${c.failed || 0} / ${t}` }));
  }));
}

export function mount() {
  const root = el("page-run");
  let timeline = null;
  let metrics = null;
  let runIndex = null;
  let timer = null;
  let lastText = "";

  const stats = h("div.stat-row.wide-stats");
  const ladder = h("section.card.ladder", { "aria-label": "Ladder" });
  const spend = h("section.card.spend", { "aria-label": "Spend" });
  const tlBody = h("div.chart-body");
  const select = h("select.run-select", { "aria-label": "Run", onchange: () => { runIndex = Number(select.value); load(); } });
  const tlSummary = h("p.page-note.tl-summary");
  const timelineCard = h("section.card.timeline-card", { "aria-label": "Goals timeline" },
    h("div.card-head", {}, h("h2", { text: "Goals on a time axis" }), select),
    h("div.legend", {}, LEGEND.map(([id, label]) => h("span.lg", {}, h("i.sw.b-" + id), label)), h("span.lg", {}, h("i.sw.sw-gap"), "Idle")),
    tlBody, tlSummary);
  const bySkill = h("section.card", { "aria-label": "Goals by skill" });
  const rungs = h("section.card", { "aria-label": "Rungs over time" });
  const hours = h("section.card", { "aria-label": "Spend per hour" });
  root.replaceChildren(
    h("div.page-head", {}, h("h1", { text: "Run" }), h("p", { text: "How far up the ladder the bot got, and how it spent its time. Empty space on the timeline is waiting." })),
    stats,
    h("div.run-grid", {}, ladder, spend),
    timelineCard,
    h("div.run-trio", {}, bySkill, rungs, hours));
  tooltipFor(tlBody);
  const tipRungs = tooltipFor(rungs);
  const tipHours = tooltipFor(hours);

  function renderState(s) {
    renderLadder(ladder, s);
    renderSpend(spend, s);
  }

  function renderStats() {
    const m = metrics;
    const idle = timeline && timeline.idle;
    const counts = {};
    for (const g of (timeline && timeline.goals) || []) counts[g.status] = (counts[g.status] || 0) + 1;
    const share = idle && idle.busy_s + idle.idle_s > 0 ? Math.round((idle.busy_s / (idle.busy_s + idle.idle_s)) * 100) : null;
    stats.replaceChildren(
      stat("Goals done", timeline ? num(counts.done || 0) : "-", timeline ? `${num(counts.failed || 0)} failed, ${num(timeline.goals.length)} in this run` : ""),
      stat("Median ticks", m && m.ticks.median !== null ? num(Math.round(m.ticks.median)) : "-", "per finished goal"),
      stat("Useful ticks", m && m.useful_goal_ticks.ratio !== null ? Math.round(m.useful_goal_ticks.ratio * 100) + "%" : "-", m ? `${num(m.useful_goal_ticks.useful)} of ${num(m.useful_goal_ticks.total)}` : ""),
      stat("Stock goals", m ? num(m.stock_goals) : "-", "gathers that raised nothing"),
      stat("Busy", share === null ? "-" : share + "%", idle ? `idle ${span(idle.idle_s)}, longest wait ${span(idle.longest_gap_s)}` : ""));
  }

  function renderTimeline() {
    if (!timeline) return;
    select.replaceChildren(...timeline.runs.filter((r) => r.goals).map((r) =>
      h("option", { value: String(r.index), selected: r.index === timeline.run, text: `${r.started_at ? new Date(r.started_at * 1000).toLocaleString("en-GB", { day: "numeric", month: "short", hour: "2-digit", minute: "2-digit" }) : "earlier"} - ${r.goals} goals` })));
    select.hidden = timeline.runs.filter((r) => r.goals).length < 2;
    const svg = goalsTimeline(timeline);
    tlBody.querySelector("svg")?.remove();
    const empty = tlBody.querySelector(".chart-empty");
    if (empty) empty.remove();
    if (svg) tlBody.prepend(svg);
    else tlBody.prepend(h("div.chart-empty.empty-state", {}, h("p", { text: "No goals yet." }), h("p.muted", { text: "Each goal the bot plays becomes a bar here, coloured by how it ended." })));
    const idle = timeline.idle;
    tlSummary.hidden = !svg;
    tlSummary.textContent = svg ? `Busy ${span(idle.busy_s)}, idle ${span(idle.idle_s)}; the longest wait was ${span(idle.longest_gap_s)}.` : "";
  }

  function renderCharts() {
    if (!metrics) return;
    bySkill.replaceChildren(h("h2", { text: "Goals by skill" }), h("p.small", { text: "done / failed / total" }), skillBars(countBySkill(timeline && timeline.goals)));
    const ladderMax = (store.state.rungs || []).length;
    const inRun = timeline ? metrics.rungs.filter((p) => p.t >= timeline.begin - 1 && p.t <= timeline.end + 1) : metrics.rungs;
    const width = Math.max(260, Math.round(rungs.clientWidth - 60)) || 360;
    const rc = rungsChart(inRun, ladderMax, width);
    rungs.querySelector("svg")?.remove();
    rungs.replaceChildren(h("h2", { text: "Rungs over time" }), h("p.small", { text: "Best ladder height after each goal" }),
      ...(rc ? [rc] : [h("p.muted.chart-empty", { text: "No finished goals with a rung count yet." })]));
    const sc = spendChart(metrics.cost_per_hour, width);
    hours.replaceChildren(h("h2", { text: "Spend per hour" }), h("p.small", { text: "All paid calls, last 48 hours" }),
      ...(sc ? [sc] : [h("p.muted.chart-empty", { text: "No paid calls recorded yet." })]));
    // replaceChildren dropped the tooltip nodes; put them back.
    rungs.append(tipRungs);
    hours.append(tipHours);
  }

  async function load() {
    try {
      const [t, m] = await Promise.all([
        getJSON("/api/goals/timeline" + (runIndex !== null ? `?run=${runIndex}` : "")),
        getJSON("/api/metrics"),
      ]);
      const text = JSON.stringify([t, m]);
      if (text === lastText) return;
      lastText = text;
      timeline = t;
      metrics = m;
      renderStats();
      renderTimeline();
      renderCharts();
    } catch { /* the link chip shows trouble */ }
  }

  renderState(store.state);
  renderStats();
  onState(renderState);
  let lastWidth = 0;
  new ResizeObserver(() => { const w = rungs.clientWidth; if (Math.abs(w - lastWidth) > 8 && w > 0) { lastWidth = w; renderCharts(); } }).observe(rungs);
  return {
    show() { renderState(store.state); load(); timer = setInterval(load, REFRESH_MS); },
    hide() { clearInterval(timer); },
  };
}
