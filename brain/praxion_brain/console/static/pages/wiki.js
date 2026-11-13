// The Wiki page: search the local Minecraft Wiki index the bot itself reads.
import { h, icon, el } from "../util.js";
import { searchWiki } from "../api.js";

const MAX_SHOWN_CHARS = 700;

function result(item) {
  const text = String(item.text || "");
  const shown = text.length > MAX_SHOWN_CHARS ? text.slice(0, MAX_SHOWN_CHARS).trimEnd() + "..." : text;
  const link = /^https?:\/\//.test(String(item.url || ""))
    ? h("a", { href: item.url, target: "_blank", rel: "noopener noreferrer", text: "Open the page" })
    : null;
  return h("article.card.wiki-hit", {},
    h("header", {}, h("h3", { text: item.title || "Untitled" }), h("span.tag", { title: "Relevance", text: `${Math.round((item.score || 0) * 100)}%` })),
    h("p", { text: shown }), link);
}

export function mount() {
  const root = el("page-wiki");
  const input = h("input", { id: "wiki-q", type: "search", maxlength: "200", placeholder: "e.g. how to craft a furnace", "aria-label": "Search the wiki", autocomplete: "off" });
  const results = h("div.wiki-results", { "aria-live": "polite" });
  const form = h("form.wiki-form", {}, input, h("button.btn.primary", { type: "submit", text: "Search" }));
  root.replaceChildren(
    h("div.page-head", {}, h("h1", { text: "Wiki" }),
      h("p", { text: "The passages the bot can look up while it plans. Search them here to see what it would find." })),
    form, results);

  let serial = 0;
  function empty(title, ...lines) {
    results.replaceChildren(h("div.empty-state", {}, icon("search"), h("p", { text: title }), ...lines.map((line) => h("p.muted", {}, line))));
  }

  function noIndex() {
    empty("No wiki index on this machine yet.",
      "Build one once, then reload this page:",
      h("code", { text: "praxion_brain wiki fetch  then  praxion_brain wiki build" }));
  }

  async function run(query) {
    const mine = ++serial;
    if (!query) { results.replaceChildren(); await probe(); return; }
    try {
      const data = await searchWiki(query);
      if (mine !== serial) return;
      if (!data.available) noIndex();
      else if (!data.results.length) empty("No passage matched that.", "Try other words, or a shorter question.");
      else results.replaceChildren(...data.results.map(result));
    } catch (error) {
      if (mine === serial) empty("The search failed.", error.message);
    }
  }

  // An empty query answers only "is there an index": the page can say so up front.
  async function probe() {
    try {
      const data = await searchWiki("");
      if (!data.available) noIndex();
      else empty("Ask the wiki something.", "Results are the best matching passages, with a relevance score.");
    } catch { empty("The wiki could not be reached."); }
  }

  form.addEventListener("submit", (event) => { event.preventDefault(); run(input.value.trim()); });
  return { show() { if (!results.children.length) probe(); input.focus(); } };
}
