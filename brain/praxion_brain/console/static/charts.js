// Plain-SVG charts for the Run page. No chart library; every mark has a text
// tooltip (`data-tip`), outcome colours come with a legend and the tooltip text,
// so colour never carries a meaning alone.
import { h, span, money, words, failureWords } from "./util.js";

const NS = "http://www.w3.org/2000/svg";

function node(name, attrs = {}, ...children) {
  const el = document.createElementNS(NS, name);
  for (const [key, value] of Object.entries(attrs)) if (value !== null && value !== undefined) el.setAttribute(key, value);
  el.append(...children.flat().filter(Boolean));
  return el;
}

const clock = (t) => new Date(t * 1000).toLocaleTimeString("en-GB", { hour: "2-digit", minute: "2-digit" });
const clockSeconds = (t) => new Date(t * 1000).toLocaleTimeString("en-GB");

/** One tooltip per chart container; it follows marks that carry `data-tip`. */
export function tooltipFor(container) {
  const tip = h("div.tip", { role: "status", hidden: true });
  container.style.position = "relative";
  container.append(tip);
  const show = (target) => {
    const text = target.getAttribute("data-tip");
    if (!text) return;
    tip.replaceChildren(...text.split("\n").map((line, i) => h(i ? "div" : "b", { text: line })));
    tip.hidden = false;
    const box = container.getBoundingClientRect();
    const rect = target.getBoundingClientRect();
    const width = tip.offsetWidth;
    const left = Math.min(Math.max(8, rect.left - box.left + rect.width / 2 - width / 2), box.width - width - 8);
    tip.style.left = `${left}px`;
    tip.style.top = `${Math.max(0, rect.top - box.top - tip.offsetHeight - 8)}px`;
  };
  const hide = () => { tip.hidden = true; };
  container.addEventListener("pointerover", (e) => { const t = e.target.closest("[data-tip]"); if (t) show(t); });
  container.addEventListener("pointerout", (e) => { if (e.target.closest("[data-tip]")) hide(); });
  container.addEventListener("focusin", (e) => { const t = e.target.closest("[data-tip]"); if (t) show(t); });
  container.addEventListener("focusout", hide);
  return tip;
}

function axisTicks(t0, t1, count) {
  const ticks = [];
  for (let i = 0; i <= count; i += 1) ticks.push(t0 + ((t1 - t0) * i) / count);
  return ticks;
}

// -- goals timeline -------------------------------------------------------- //
const LANE_H = 34;
const MAX_LANES = 8;
const W = 1000;
const LEFT = 150;
const RIGHT = 16;

/** Goals as bars on a time axis, one lane per skill; idle gaps stay empty (hatched). */
export function goalsTimeline(timeline) {
  const goals = timeline.goals || [];
  const begin = timeline.begin;
  const end = timeline.end;
  if (!goals.length || begin === null || end === null || end <= begin) return null;
  const order = [];
  for (const goal of goals) if (!order.includes(goal.skill || "(none)")) order.push(goal.skill || "(none)");
  const counts = Object.fromEntries(order.map((skill) => [skill, goals.filter((g) => (g.skill || "(none)") === skill).length]));
  const lanes = order.length > MAX_LANES ? [...order].sort((a, b) => counts[b] - counts[a]).slice(0, MAX_LANES - 1).concat("(other)") : order;
  const laneOf = (goal) => { const s = goal.skill || "(none)"; const i = lanes.indexOf(s); return i >= 0 ? i : lanes.length - 1; };
  const plotW = W - LEFT - RIGHT;
  const x = (t) => LEFT + ((t - begin) / (end - begin)) * plotW;
  const height = lanes.length * LANE_H + 34;
  const svg = node("svg", { viewBox: `0 0 ${W} ${height}`, class: "chart-svg timeline", role: "img", "aria-label": `Goals over time, ${goals.length} goals in ${lanes.length} lanes` },
    node("defs", {}, node("pattern", { id: "hatch", width: 6, height: 6, patternUnits: "userSpaceOnUse", patternTransform: "rotate(45)" }, node("line", { x1: 0, y1: 0, x2: 0, y2: 6, class: "hatch-line" }))));
  // lanes
  lanes.forEach((name, i) => {
    svg.append(node("rect", { x: LEFT, y: i * LANE_H, width: plotW, height: LANE_H, class: i % 2 ? "lane odd" : "lane" }));
    const label = node("text", { x: LEFT - 10, y: i * LANE_H + LANE_H / 2 + 4, class: "lane-label", "text-anchor": "end" });
    label.textContent = name.length > 20 ? name.slice(0, 19) + "…" : name;
    svg.append(label);
  });
  // idle gaps, across every lane
  for (const gap of timeline.gaps || []) {
    const gx = x(gap.t0);
    const gw = Math.max(0, x(gap.t1) - gx);
    if (gw < 1.5) continue;
    svg.append(node("rect", { x: gx, y: 0, width: gw, height: lanes.length * LANE_H, class: "gap", "data-tip": `Idle for ${span(gap.t1 - gap.t0)}\n${clockSeconds(gap.t0)} to ${clockSeconds(gap.t1)}\nNo goal was running.`, tabindex: gw > 12 ? "0" : null }));
    if (gw > 64) {
      const text = node("text", { x: gx + gw / 2, y: lanes.length * LANE_H + 14, class: "gap-label", "text-anchor": "middle" });
      text.textContent = `idle ${span(gap.t1 - gap.t0)}`;
      svg.append(text);
    }
  }
  // goal bars
  for (const goal of goals) {
    const gx = x(goal.t0);
    const stop = goal.t1 !== null ? goal.t1 : end;
    const gw = Math.max(3, x(stop) - gx);
    const lane = laneOf(goal);
    const tip = [
      `${goal.skill || "(none)"} - ${goal.status}`,
      goal.condition ? `Goal ${goal.condition}` : "",
      `${clockSeconds(goal.t0)}, ${goal.t1 !== null ? span(goal.t1 - goal.t0) : "still running"}`,
      goal.failure_kind ? `Failed: ${failureWords(goal.failure_kind)}` : "",
      goal.ticks != null ? `${goal.ticks} ticks, ${goal.rungs_met ?? "?"} rungs` : "",
    ].filter(Boolean).join("\n");
    svg.append(node("rect", { x: gx, y: lane * LANE_H + 6, width: gw, height: LANE_H - 12, rx: 3, class: `bar b-${goal.status}`, "data-tip": tip, tabindex: "0", role: "img", "aria-label": tip.replace(/\n/g, ", ") }));
  }
  // time axis
  const axisY = lanes.length * LANE_H;
  for (const t of axisTicks(begin, end, 6)) {
    svg.append(node("line", { x1: x(t), x2: x(t), y1: 0, y2: axisY, class: "grid" }));
    const label = node("text", { x: x(t), y: axisY + 28, class: "axis-label", "text-anchor": t === begin ? "start" : t === end ? "end" : "middle" });
    label.textContent = clock(t);
    svg.append(label);
  }
  return svg;
}

// -- rungs over time -------------------------------------------------------- //
const H2 = 170;

/** The best ladder height reached after each goal, as a step line. */
export function rungsChart(points, max, width = W) {
  const usable = points.filter((p) => p.t);
  if (!usable.length) return null;
  const t0 = usable[0].t;
  const t1 = usable[usable.length - 1].t;
  const spanT = Math.max(1, t1 - t0);
  const plotW = width - 56 - RIGHT;
  const x = (t) => 56 + ((t - t0) / spanT) * plotW;
  const top = Math.max(max || 0, ...usable.map((p) => p.rungs), 1);
  const y = (v) => H2 - 30 - (v / top) * (H2 - 50);
  const svg = node("svg", { viewBox: `0 0 ${width} ${H2}`, class: "chart-svg", role: "img", "aria-label": `Rungs met over time, now ${usable[usable.length - 1].rungs} of ${top}` });
  for (let v = 0; v <= top; v += top > 6 ? 3 : 1) {
    svg.append(node("line", { x1: 56, x2: width - RIGHT, y1: y(v), y2: y(v), class: "grid" }));
    const label = node("text", { x: 46, y: y(v) + 4, class: "axis-label", "text-anchor": "end" });
    label.textContent = String(v);
    svg.append(label);
  }
  const marks = usable.map((p) =>
    node("circle", { cx: x(p.t), cy: y(p.rungs), r: 4, class: "dot-mark", tabindex: "0", "data-tip": `${p.rungs} rungs met\nafter ${p.goal_id || "a goal"}\n${clockSeconds(p.t)}` }));
  // A step line: hold the previous height until the next goal.
  let path = `M${x(t0)} ${y(usable[0].rungs)}`;
  for (const p of usable.slice(1)) path += ` H${x(p.t)} V${y(p.rungs)}`;
  path += ` H${width - RIGHT}`;
  svg.append(node("path", { d: path, class: "step-line" }), ...marks);
  for (const t of [t0, t1]) {
    const label = node("text", { x: x(t), y: H2 - 8, class: "axis-label", "text-anchor": t === t0 ? "start" : "end" });
    label.textContent = clock(t);
    svg.append(label);
  }
  return svg;
}

// -- spend per hour --------------------------------------------------------- //
/** Spend per hour, one thin rounded bar each; the peak is labelled. */
export function spendChart(hours, width = W) {
  if (!hours.length) return null;
  const peak = Math.max(...hours.map((b) => b.cost), 1e-9);
  const plotW = width - 56 - RIGHT;
  const slot = plotW / hours.length;
  const bw = Math.min(26, Math.max(4, slot * 0.6));
  const svg = node("svg", { viewBox: `0 0 ${width} ${H2}`, class: "chart-svg", role: "img", "aria-label": `Spend per hour, peak ${money(peak)}` });
  for (const f of [0, 0.5, 1]) {
    const yy = H2 - 30 - f * (H2 - 50);
    svg.append(node("line", { x1: 56, x2: width - RIGHT, y1: yy, y2: yy, class: "grid" }));
    const label = node("text", { x: 46, y: yy + 4, class: "axis-label", "text-anchor": "end" });
    label.textContent = money(peak * f);
    svg.append(label);
  }
  hours.forEach((b, i) => {
    const bh = b.cost > 0 ? Math.max(2, (b.cost / peak) * (H2 - 50)) : 0;
    const bx = 56 + i * slot + (slot - bw) / 2;
    const hour = new Date(b.t * 1000);
    const tip = `${hour.toLocaleDateString("en-GB", { day: "numeric", month: "short" })} ${String(hour.getHours()).padStart(2, "0")}:00\n${money(b.cost)}, ${b.calls} calls`;
    if (bh) svg.append(node("rect", { x: bx, y: H2 - 30 - bh, width: bw, height: bh, rx: Math.min(4, bw / 2), class: "bar b-spend", "data-tip": tip, tabindex: "0" }));
    else svg.append(node("rect", { x: bx, y: H2 - 32, width: bw, height: 2, class: "bar b-none", "data-tip": tip }));
  });
  const first = new Date(hours[0].t * 1000);
  const last = new Date(hours[hours.length - 1].t * 1000);
  const fmt = (d) => `${d.toLocaleDateString("en-GB", { day: "numeric", month: "short" })} ${String(d.getHours()).padStart(2, "0")}:00`;
  const a = node("text", { x: 56, y: H2 - 8, class: "axis-label" }); a.textContent = fmt(first);
  const b = node("text", { x: width - RIGHT, y: H2 - 8, class: "axis-label", "text-anchor": "end" }); b.textContent = fmt(last);
  svg.append(a, b);
  return svg;
}

export { words };
