// The Skills page: core, learned and candidate skills, rules as formatted code.
import { h, icon, el, words, recall, remember } from "../util.js";
import { getJSON } from "../api.js";
import { tint, practices } from "../inspector.js";

const GROUPS = [
  ["core", "Core", "Written by hand, shipped with the body."],
  ["learned", "Learned", "Written by the model, admitted after practice runs."],
  ["candidates", "Candidates", "Drafted but not admitted (yet)."],
];
const REFRESH_MS = 15000;

function formatted(source) {
  const pre = h("pre.code.rules");
  const lines = String(source || "").split("\n");
  pre.append(...lines.map((line, i) => {
    const row = h("span.ln");
    row.append(h("span.no", { text: String(i + 1) }), tint(line));
    return row;
  }));
  return pre;
}

export function mount(ctx) {
  const root = el("page-skills");
  let data = { core: [], learned: [], candidates: [] };
  let group = recall("skills.group", "core");
  let selected = null;
  let timer = null;
  let lastText = "";

  const tabs = h("div.seg", { role: "tablist", "aria-label": "Skill group" });
  const list = h("div.skill-list", { role: "listbox", "aria-label": "Skills" });
  const detail = h("section.card.skill-detail", { "aria-live": "polite" });
  root.replaceChildren(
    h("div.page-head", {}, h("h1", { text: "Skills" }), h("p", { text: "What the bot can already do, and what it is learning. Select a skill to read its rules." })),
    tabs, h("div.skills-grid", {}, h("div.card.skill-pane", {}, list), detail));

  const keyOf = (item) => (group === "candidates" ? item.skill : item.name);

  function showDetail(item) {
    if (!item) {
      detail.replaceChildren(h("div.empty-state", {}, icon("box"), h("p", { text: "Nothing here yet." }),
        h("p.muted", { text: group === "candidates" ? "A skill the model drafts shows up here, with every admission attempt." : "Select a skill on the left." })));
      return;
    }
    if (group === "candidates") {
      const last = item.attempts[item.attempts.length - 1];
      detail.replaceChildren(...[
        h("h2", {}, words(item.skill)),
        h("div.meta", {}, h("span.chip", { text: item.status }), h("span.chip", { text: `${item.attempts.length} attempts` }), h("span.chip", { text: `${item.repairs_used ?? 0} repairs` })),
        item.reason ? h("p.muted", { text: words(item.reason) }) : null,
        last && last.source ? h("div.block", {}, h("h4", { text: "Latest candidate" }), formatted(last.source)) : null,
        practices(item)].filter(Boolean));
      return;
    }
    detail.replaceChildren(...[
      h("h2", {}, item.name),
      item.doc ? h("p.skill-doc", { text: item.doc }) : null,
      h("div.meta", {}, h("span.chip", { text: `${item.rules} rules` }), h("span.chip", { text: item.params.length ? `params: ${item.params.join(", ")}` : "no params" }),
        h("span.chip" + (item.report ? ".learned" : ""), { text: item.report ? "written by the model" : "core" })),
      h("div.block", {}, h("h4", { text: "Rules" }), formatted(item.source)),
      item.report ? practices(item.report) : null].filter(Boolean));
  }

  function render() {
    tabs.replaceChildren(...GROUPS.map(([id, label]) =>
      h("button.seg-btn", { type: "button", role: "tab", "aria-selected": String(id === group), onclick: () => { group = id; selected = null; remember("skills.group", id); render(); } },
        label, h("span.count", { text: String((data[id] || []).length) }))));
    const items = data[group] || [];
    const hint = GROUPS.find(([id]) => id === group)[2];
    if (!items.some((item) => keyOf(item) === selected)) selected = items[0] ? keyOf(items[0]) : null;
    list.replaceChildren(
      h("p.pane-hint", { text: hint }),
      ...(items.length ? items.map((item) => {
        const key = keyOf(item);
        const bad = group === "candidates" && item.status !== "admitted";
        return h("button.skill-row" + (key === selected ? ".on" : "") + (bad ? ".bad" : ""), {
          type: "button", role: "option", "aria-selected": String(key === selected), title: item.doc || item.reason || "",
          onclick: () => { selected = key; render(); },
        }, h("span.skill-name", { text: key }),
          group === "candidates" ? h("span.tag" + (bad ? ".warn" : ""), { text: item.status })
            : h("span.small", { text: `${item.rules} rules` }));
      }) : [h("p.muted.pane-empty", { text: group === "learned" ? "Nothing admitted yet." : "None." })]));
    showDetail(items.find((item) => keyOf(item) === selected));
  }

  async function load() {
    try {
      const next = await getJSON("/api/skills");
      const text = JSON.stringify(next);
      if (text === lastText) return; // an unchanged answer must not collapse what the viewer opened
      lastText = text;
      data = next;
      render();
    } catch { /* the link chip shows trouble */ }
  }

  render();
  return {
    show() { load(); timer = setInterval(load, REFRESH_MS); },
    hide() { clearInterval(timer); },
  };
}
