// The inventory card: the bot's own container as a slot grid.
//
// Tiles are lettered monograms until the local client's texture arrives, then
// a real icon (26.3 client jar, `/api/item-icon/<id>`): a sprite for generated
// items, an isometric three-face cube for blocks. The monogram stays in the
// DOM as the accessible text, and a missing or failed icon simply keeps it.
// Slot numbers follow vanilla's InventoryMenu, the layout the body stores
// (PLAYER_CONTAINER_SIZE = 46): 0 result, 1-4 grid, 5-8 armour, 9-35 storage,
// 36-44 hotbar, 45 offhand.
import { h, el, itemGlyph, STALE_SECONDS } from "./util.js";
import { getJSON } from "./api.js";

const STORAGE = slotRange(9, 35);
const HOTBAR = slotRange(36, 44);
const ARMOUR = [5, 6, 7, 8];
const ARMOUR_LABELS = ["helmet", "chestplate", "leggings", "boots"];
const OFFHAND = 45;
const LEGEND_ROWS = 12;
const ICON_PATH = "/api/item-icon/";
const MAX_ICONS = 512;
const MAX_LAYERS = 8;
const VANILLA_ID = /^(?:minecraft:)?[a-z0-9_]+$/;

// One entry per item id, set *before* the request starts so a redraw during a
// load never starts a second fetch. `state` is "loading" | "ready" | "missing".
const icons = new Map();

let view = null;

function slotRange(first, last) {
  const out = [];
  for (let slot = first; slot <= last; slot += 1) out.push(slot);
  return out;
}

function build() {
  const counts = h("span");
  const age = h("span.inv-age");
  const meta = h("span.inv-meta", {}, counts, age);
  const storage = h("div.slots");
  const hotbar = h("div.slots");
  const armour = h("div.armour");
  const legend = h("div.inv-legend");
  const aggregate = h("div.inv-aggregate");
  const board = h("div.inv-board", {},
    h("div.inv-label", { text: "Storage 9-35" }), storage,
    h("div.inv-label", { text: "Hotbar 36-44, selected outlined" }), hotbar);
  const side = h("div.inv-side", {},
    h("div.inv-label", { text: "Armour 5-8 and offhand 45" }), armour,
    h("div.inv-label", { text: "Items" }), legend);
  const root = el("inventory");
  root.replaceChildren(
    h("h2", {}, "Inventory", meta),
    h("div.inv-body", {}, board, aggregate, side),
  );
  view = { root, counts, age, storage, hotbar, armour, legend, aggregate, signature: null };
}

function entries(inventory) {
  return Object.entries(inventory).sort((a, b) => Number(b[1]) - Number(a[1]));
}

function row([name, count]) {
  return h("div.inv-row", {},
    h("span.inv-name", { text: String(name).replace(/_/g, " ") }),
    h("span.inv-count", { text: count }));
}

// -- icons ---------------------------------------------------------------- //

const PNG_DATA_URL = /^data:image\/png;base64,[A-Za-z0-9+/]+={0,2}$/;

function png(url) {
  return typeof url === "string" && PNG_DATA_URL.test(url) ? url : null;
}

/** Validate the server payload; anything unexpected keeps the monogram. */
function normaliseIcon(data) {
  if (!data || typeof data !== "object") return null;
  if (data.kind === "sprite" && Array.isArray(data.layers)) {
    const layers = data.layers.map(png).filter(Boolean).slice(0, MAX_LAYERS);
    return layers.length ? { kind: "sprite", layers } : null;
  }
  if (data.kind === "cube") {
    const top = png(data.top);
    const left = png(data.left);
    const right = png(data.right);
    return top && left && right ? { kind: "cube", top, left, right } : null;
  }
  return null;
}

function paint(node, data) {
  const holder = node.querySelector(".inv-icon");
  if (!holder) return;
  holder.replaceChildren();
  if (data.kind === "cube") {
    holder.className = "inv-icon cube";
    for (const face of ["top", "left", "right"]) {
      holder.append(h(`span.face.${face}`, {
        style: `background-image:url("${data[face]}")`,
      }));
    }
  } else {
    holder.className = "inv-icon sprite";
    // CSS paints the first background on top, so the base layer goes last.
    const paintOrder = [...data.layers].reverse();
    holder.style.backgroundImage = paintOrder.map((url) => `url("${url}")`).join(", ");
    holder.style.backgroundSize = paintOrder.map(() => "contain").join(", ");
    holder.style.backgroundRepeat = paintOrder.map(() => "no-repeat").join(", ");
  }
  node.classList.add("has-icon");
  node.classList.remove("loading");
}

/** Paint every live tile for the id; a tile whose item changed is skipped. */
function paintItem(item, data) {
  if (!view) return;
  for (const node of view.root.querySelectorAll(".slot[data-item]")) {
    if (node.dataset.item === item) paint(node, data);
  }
}

/** A resolved miss keeps the normal monogram: drop the in-flight dim only. */
function settleMiss(item) {
  if (!view) return;
  for (const node of view.root.querySelectorAll(".slot[data-item]")) {
    if (node.dataset.item === item) node.classList.remove("loading");
  }
}

function trimIcons(keep) {
  for (const [key, entry] of icons) {
    if (icons.size <= MAX_ICONS) break;
    if (key !== keep && entry.state !== "loading") icons.delete(key);
  }
}

function requestIcon(item) {
  const entry = { state: "loading", data: null };
  icons.set(item, entry);
  trimIcons(item);
  getJSON(ICON_PATH + encodeURIComponent(item))
    .then((data) => {
      const normal = normaliseIcon(data);
      entry.state = normal ? "ready" : "missing";
      entry.data = normal;
      if (normal) paintItem(item, normal);
      else settleMiss(item);
    })
    .catch(() => {
      // A missing route (older server) or a 404 is a normal fallback state.
      entry.state = "missing";
      entry.data = null;
      settleMiss(item);
    });
}

// -- rendering ------------------------------------------------------------ //

function tile(slot, selected = false, label = "") {
  if (!slot) {
    // The selected slot is outlined even when it is empty: the outline marks
    // the hand, and an empty hand is a normal state (review finding 1).
    // role="img" gives the slot an accessible name: the title tooltip alone
    // is not exposed to screen readers (GLM inventory review).
    const name = (label ? `${label}: ` : "") + "empty" + (selected ? ", selected" : "");
    return h("div.slot.empty" + (selected ? ".selected" : ""), {
      role: "img",
      "aria-label": name,
      title: label ? `${label}: empty` : "empty",
    });
  }
  const item = String(slot.item);
  const glyph = itemGlyph(item);
  const name = String(item).replace(/_/g, " ");
  const children = [
    h("span.glyph", { "aria-hidden": "true", text: glyph.text }),
    h("span.inv-icon", { "aria-hidden": "true" }),
  ];
  if (Number(slot.count) > 1) children.push(h("span.count", { "aria-hidden": "true", text: slot.count }));
  const title = `${name} ×${slot.count}${label ? ` (${label})` : ""}`;
  const node = h("div.slot" + (selected ? ".selected" : ""), {
    role: "img",
    "aria-label": title + (selected ? ", selected" : ""),
    title,
    style: `--tile-hue:${glyph.hue}`,
  }, children);
  node.dataset.item = item;
  const cached = VANILLA_ID.test(item) ? icons.get(item) : null;
  if (cached && cached.state === "ready") {
    paint(node, cached.data);
  } else if (VANILLA_ID.test(item) && !cached) {
    requestIcon(item);
    node.classList.add("loading");
  } else if (cached) {
    node.classList.add("loading");
  }
  return node;
}

function updateAge(snapshotAt) {
  if (snapshotAt === null) {
    view.age.textContent = "no snapshot";
    view.root.classList.remove("stale");
    return;
  }
  const age = Math.max(0, Date.now() / 1000 - snapshotAt);
  view.age.textContent = `facts ${Math.round(age)} s old`;
  view.root.classList.toggle("stale", age > STALE_SECONDS);
}

/** Render one state event: `facts` is `state.facts` and `snapshotAt` is
 * `state.snapshot_at` (wall-clock seconds). Cheap when nothing changed, so a
 * 1 Hz refresh does not rebuild or re-animate the grid. */
export function renderInventory(facts, snapshotAt) {
  if (!view) build();
  const f = facts || null;
  const at = Number.isFinite(snapshotAt) ? snapshotAt : null;
  const slots = f && Array.isArray(f.slots) ? f.slots : [];
  const inventory = (f && f.inventory) || {};
  const types = Object.keys(inventory).length;
  const total = Object.values(inventory).reduce((sum, n) => sum + Number(n || 0), 0);
  view.counts.textContent = f ? `${types} types · ${total} items` : "no snapshot yet";

  // An old body sends totals but no slots: show the aggregate list instead of
  // 36 empty tiles, so a missing field never looks like an empty inventory.
  const aggregate = slots.length === 0 && types > 0;
  const selected = f && Number.isInteger(f.selected_hotbar_slot) ? f.selected_hotbar_slot : null;
  // `selected_hotbar_slot` is the hand index 0..=8 (port `wire.rs` "The hand,
  // 0..=8"), not a container slot: hand 0 is hotbar slot 36. Comparing a
  // container slot to the hand directly never matches (review finding 1).
  const selectedSlot = selected === null ? null : HOTBAR[0] + selected;
  const signature = f
    ? (aggregate ? "aggregate:" + JSON.stringify(inventory)
      : JSON.stringify(slots) + "|" + (selected === null ? "" : selected))
    : "none";
  if (view.signature !== signature) {
    view.signature = signature;
    // The mode is a root class, never the `hidden` attribute: this panel's
    // boxes set `display: grid`, and an author display rule beats the UA's
    // `[hidden] { display: none }`, so a hidden box stayed visible (review
    // finding 3). app.css keys both directions off `.inventory.aggregate`.
    view.root.classList.toggle("aggregate", aggregate);
    if (aggregate) {
      view.aggregate.replaceChildren(...entries(inventory).map(row));
    } else {
      const bySlot = new Map(slots.map((slot) => [slot.slot, slot]));
      view.storage.replaceChildren(...STORAGE.map((slot) => tile(bySlot.get(slot))));
      view.hotbar.replaceChildren(...HOTBAR.map((slot) => tile(bySlot.get(slot), slot === selectedSlot)));
      view.armour.replaceChildren(
        ...ARMOUR.map((slot, index) => tile(bySlot.get(slot), false, ARMOUR_LABELS[index])),
        tile(bySlot.get(OFFHAND), false, "offhand"),
      );
      const listed = entries(inventory);
      // Built into a list first: a variadic node list stringifies a null child
      // to the text "null" instead of skipping it, so the "+N more" row is
      // pushed rather than left as a placeholder (review finding 2).
      const rows = listed.slice(0, LEGEND_ROWS).map(row);
      if (listed.length > LEGEND_ROWS) {
        rows.push(h("div.inv-row.more", { text: `+${listed.length - LEGEND_ROWS} more` }));
      }
      view.legend.replaceChildren(...rows);
    }
  }
  updateAge(at);
}
