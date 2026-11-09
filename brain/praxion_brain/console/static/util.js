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
