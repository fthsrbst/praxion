// Small DOM and formatting helpers shared by the console modules.

const SVG = "http://www.w3.org/2000/svg";

/** A snapshot older than this reads as stale: the console clears it, so the
 * one-second refresh has missed several ticks (decision 2026-11-09 "Console":
 * a page refreshes the bot's state every second). One value for the hero's
 * "facts N s old" and the inventory's dimming, so a cadence change moves both.
 */
export const STALE_SECONDS = 5;

/** h("div.card", {onclick}, child, "text") builds an element. */
export function h(tag, attrs, ...children) {
  const [name, ...classes] = tag.split(".");
  const node = document.createElement(name || "div");
  if (classes.length) node.className = classes.join(" ");
  for (const [key, value] of Object.entries(attrs || {})) {
    if (value === null || value === undefined || value === false) continue;
    if (key.startsWith("on")) node.addEventListener(key.slice(2), value);
    else if (key === "text") node.textContent = value;
    else node.setAttribute(key, value === true ? "" : value);
  }
  node.append(...children.flat().filter((c) => c !== null && c !== undefined && c !== false));
  return node;
}

export function svg(markup, className) {
  const wrap = document.createElementNS(SVG, "svg");
  wrap.setAttribute("viewBox", "0 0 24 24");
  // An inline SVG with only a viewBox has no intrinsic size: in an unsized
  // container it takes the whole flex line (measured 1138x1138) or collapses
  // to 0x0. Every existing CSS rule overrides these attributes; an unsized
  // insertion point now renders 18 px instead.
  wrap.setAttribute("width", "18");
  wrap.setAttribute("height", "18");
  wrap.setAttribute("fill", "none");
  wrap.setAttribute("stroke", "currentColor");
  wrap.setAttribute("stroke-width", "1.7");
  wrap.setAttribute("stroke-linecap", "round");
  wrap.setAttribute("stroke-linejoin", "round");
  wrap.setAttribute("aria-hidden", "true");
  wrap.setAttribute("class", className ? "icon " + className : "icon");
  wrap.innerHTML = markup;
  return wrap;
}

// Slightly wobbly strokes: the hand-drawn feel.
const ICONS = {
  brain: '<path d="M9 4.5C6.5 4 4.8 6 5.3 8 3.8 9 3.8 11.3 5 12.3c-.9 1.6.1 3.6 2 3.9.3 1.9 2.3 3.2 4 2.2V5.6C10.6 4.6 9.8 4.4 9 4.5Z"/><path d="M15 4.5c2.5-.5 4.2 1.5 3.7 3.5 1.5 1 1.5 3.3.3 4.3.9 1.6-.1 3.6-2 3.9-.3 1.9-2.3 3.2-4 2.2"/>',
  route: '<circle cx="6" cy="18" r="2.2"/><circle cx="18" cy="6" r="2.2"/><path d="M8 17.5c6-.5 1-10 8-11"/>',
  pen: '<path d="M4 20l1-4L16.5 4.5a2 2 0 0 1 3 3L8 19l-4 1Z"/><path d="M14.5 6.5l3 3"/>',
  play: '<path d="M7 4.8v14.4a.8.8 0 0 0 1.2.7l11-7.2a.8.8 0 0 0 0-1.4L8.2 4.1A.8.8 0 0 0 7 4.8Z"/>',
  pause: '<path d="M8 5v14M16 5v14"/>',
  flag: '<path d="M5 21V4"/><path d="M5 5c4-2 6 2 10 0 1.5-.7 3-.7 4 0v8c-1-.7-2.500-.7-4 0-4 2-6-2-10 0"/>',
  check: '<path d="M4.5 12.5l5 5L19.500 6.500"/>',
  cross: '<path d="M6 6l12 12M18 6L6 18"/>',
  coin: '<circle cx="12" cy="12" r="8.500"/><path d="M12 7.500v9M9.500 10c0-1.200 1-1.800 2.500-1.800s2.500.6 2.500 1.700c0 2.300-5 1.100-5 3.500 0 1.100 1 1.800 2.500 1.800s2.500-.7 2.500-1.800"/>',
  spark: '<path d="M12 3c.5 4.500 2.500 6.500 7 7-4.500.5-6.500 2.500-7 7-.5-4.500-2.500-6.500-7-7 4.500-.5 6.500-2.500 7-7Z"/>',
  dot: '<circle cx="12" cy="12" r="2.500"/>',
  stop: '<rect x="6" y="6" width="12" height="12" rx="3"/>',
  alert: '<path d="M12 4l9 16H3L12 4Z"/><path d="M12 10v4M12 17v.1"/>',
  user: '<circle cx="12" cy="8" r="3.500"/><path d="M5 20c.5-4 3.500-6 7-6s6.500 2 7 6"/>',
  box: '<path d="M4 8l8-4 8 4v8l-8 4-8-4V8Z"/><path d="M4 8l8 4 8-4M12 12v8"/>',
  close: '<path d="M6 6l12 12M18 6L6 18"/>',
  skull: '<path d="M12 3.500c-4 0-7 2.800-7 6.600 0 2.200 1 3.800 2.500 4.800V18h9v-3.100c1.500-1 2.500-2.600 2.500-4.800 0-3.800-3-6.600-7-6.600Z"/><circle cx="9.200" cy="10.500" r="1.300"/><circle cx="14.800" cy="10.500" r="1.300"/><path d="M10 18v2.500M14 18v2.500"/>',
  shield: '<path d="M12 3.500l7 2.500v5.500c0 4.200-3 7.600-7 9-4-1.400-7-4.800-7-9V6l7-2.500Z"/><path d="M9 12l2.200 2.200L15.500 10"/>',
  moon: '<path d="M19 14.500A7.500 7.500 0 0 1 9.500 5 7.500 7.500 0 1 0 19 14.500Z"/>',
  chat: '<path d="M4.500 5h15a1.500 1.500 0 0 1 1.500 1.500v9a1.500 1.500 0 0 1-1.500 1.500H11l-4.500 3.500V17H4.500A1.500 1.500 0 0 1 3 15.500v-9A1.500 1.500 0 0 1 4.500 5Z"/>',
  search: '<circle cx="11" cy="11" r="6.500"/><path d="M16 16l4.500 4.500"/>',
  eye: '<path d="M2.500 12S6 5.500 12 5.500 21.500 12 21.500 12 18 18.500 12 18.500 2.500 12 2.500 12Z"/><circle cx="12" cy="12" r="2.800"/>',
  clock: '<circle cx="12" cy="12" r="8.500"/><path d="M12 7v5l3 2"/>',
  arrow: '<path d="M12 5v14M6 13l6 6 6-6"/>',
  chevron: '<path d="M9 6l6 6-6 6"/>',
  bolt: '<path d="M13 3L5 13.500h6L10 21l8-10.500h-6L13 3Z"/>',
};

export function icon(name, className) {
  return svg(ICONS[name] || ICONS.dot, className);
}

export function money(value) {
  if (value === null || value === undefined) return "-";
  if (value === 0) return "$0";
  return value < 0.01 ? "$" + value.toFixed(5) : "$" + value.toFixed(value < 1 ? 3 : 2);
}

export function num(value) {
  return value === null || value === undefined ? "-" : Number(value).toLocaleString("en-US");
}

export function ago(epochSeconds) {
  if (!epochSeconds) return "";
  const s = Math.max(0, Date.now() / 1000 - epochSeconds);
  if (s < 60) return Math.floor(s) + "s ago";
  if (s < 3600) return Math.floor(s / 60) + "m ago";
  return Math.floor(s / 3600) + "h ago";
}

export function clock(iso) {
  const d = new Date(iso);
  return isNaN(d) ? "" : d.toLocaleTimeString("en-GB");
}

export function sentence(text) {
  const t = String(text || "").trim();
  return t ? t[0].toUpperCase() + t.slice(1) : "";
}

/** A smooth polyline path through points in a w x h box (values 0..max). */
export function sparkPath(values, w, h, pad = 4) {
  if (!values.length) return "";
  const max = Math.max(...values, 1e-9);
  const step = values.length > 1 ? (w - pad * 2) / (values.length - 1) : 0;
  return values
    .map((v, i) => `${i ? "L" : "M"}${(pad + i * step).toFixed(1)} ${(h - pad - (v / max) * (h - pad * 2)).toFixed(1)}`)
    .join(" ");
}

export function el(id) {
  return document.getElementById(id);
}

/** A lettered tile for an item id: `oak_log` -> `{text: "OL", hue: 23}`.
 *
 * Tiles are monograms shipped with the console, never Mojang textures
 * (decision 2026-11-09 "Console": items show as lettered tiles). The hue is a
 * stable arbitrary tint that only makes neighbouring tiles distinguishable; it
 * does not encode the item's material.
 */
export function itemGlyph(id) {
  const name = String(id || "").replace(/^minecraft:/, "");
  const words = name.split(/[_ ]+/).filter(Boolean);
  const text = (words.length > 1 ? words[0][0] + words[words.length - 1][0]
    : (words[0] || "?").slice(0, 2)).toUpperCase();
  let hash = 0;
  for (const ch of name) hash = (hash * 31 + ch.codePointAt(0)) >>> 0;
  return { text, hue: hash % 360 };
}

/** An ISO timestamp as POSIX seconds, or ``null``. */
export function epoch(iso) {
  const t = Date.parse(iso);
  return Number.isNaN(t) ? null : t / 1000;
}

/** Seconds as "12s", "3m 05s" or "1h 02m". */
export function span(seconds) {
  if (seconds === null || seconds === undefined || !Number.isFinite(seconds)) return "-";
  const s = Math.max(0, Math.round(seconds));
  if (s < 60) return s + "s";
  if (s < 3600) return Math.floor(s / 60) + "m " + String(s % 60).padStart(2, "0") + "s";
  return Math.floor(s / 3600) + "h " + String(Math.floor((s % 3600) / 60)).padStart(2, "0") + "m";
}

/** "12s ago" style label for a POSIX time; the feed refreshes it on a timer. */
export function relative(epochSeconds, now = Date.now() / 1000) {
  if (!epochSeconds) return "";
  const s = Math.max(0, now - epochSeconds);
  if (s < 5) return "just now";
  if (s < 60) return Math.floor(s) + "s ago";
  if (s < 3600) return Math.floor(s / 60) + "m ago";
  if (s < 86400) return Math.floor(s / 3600) + "h ago";
  return Math.floor(s / 86400) + "d ago";
}

export function words(snake) {
  return String(snake || "").replace(/^minecraft:/, "").replace(/_/g, " ").trim();
}

/** Why a goal failed, in plain words. Unknown kinds fall back to the kind itself. */
const FAILURES = {
  no_block: "found no block of that kind nearby",
  no_blocks: "found no matching blocks around",
  out_of_reach: "the target was out of reach",
  no_sight: "had no line of sight to the target",
  not_found: "could not find what it was looking for",
  unreachable: "found no path to the target",
  unconfirmed: "the body never confirmed the result",
  inconclusive_body: "the body gave no clear answer",
  loop_error: "an internal error stopped the loop",
  died: "the bot died",
  needs_table: "it needs a crafting table first",
  needs_tool: "it needs a better tool first",
  no_raw_iron: "there was no raw iron to smelt",
  no_fuel: "there was no fuel",
  no_furnace: "there was no furnace",
  no_item: "an ingredient was missing",
  unsafe_drop: "the drop was not safe",
  fluid: "water or lava was in the way",
  too_deep: "the stair would have gone too deep",
  underwater: "the eyes were under water and the breath would not last",
  no_progress: "the same step kept restarting without progress",
  timeout: "it ran out of time",
  budget: "it ran out of its tick budget",
  stuck: "the bot got stuck",
  no_way_up: "no way up to the surface could be cut",
  skill_wrong: "the skill did not fit the goal",
  bad_args: "the skill got bad arguments",
  preempted_by_survival: "survival took over",
  health_low: "health was too low",
  not_huntable: "that mob cannot be hunted",
  desync: "the client and the server drifted apart",
};

export function failureWords(kind) {
  if (!kind) return "";
  return FAILURES[kind] || words(kind);
}

/** Per-viewer preferences: never required, so every access is guarded. */
export function recall(key, fallback) {
  try {
    const value = localStorage.getItem("praxion.console." + key);
    return value === null ? fallback : value;
  } catch {
    return fallback;
  }
}

export function remember(key, value) {
  try {
    localStorage.setItem("praxion.console." + key, value);
  } catch {
    /* private mode: the choice lasts for this page only */
  }
}
