// The inspector drawer: an LLM/Jev exchange, a skill's rules, an admission report.
import { h, icon, el, money, num } from "./util.js";
import { getJSON } from "./api.js";

let returnFocus = null;

export function isOpen() {
  return !el("drawer").hidden;
}

export function closeDrawer() {
  el("drawer").hidden = true;
  el("scrim").hidden = true;
  if (returnFocus) returnFocus.focus();
}

function show(title, subtitle, ...content) {
  const drawer = el("drawer");
  if (drawer.hidden) returnFocus = document.activeElement;
  drawer.replaceChildren(
    h("div.drawer-head", {}, h("div", {}, h("h3", { text: title }), subtitle ? h("div.small", { text: subtitle }) : null),
      h("button.x", { type: "button", "aria-label": "Close", onclick: closeDrawer }, icon("close"))),
    ...content.filter(Boolean),
  );
  drawer.hidden = false;
  el("scrim").hidden = false;
  drawer.scrollTop = 0;
  drawer.focus();
}

const KEYWORDS = /^(skill|rules|doc|do|done|true|false|and|or|not)$/;
/** The rule DSL as spans: keywords, strings, numbers, comments. */
export function tint(source) {
  const out = document.createDocumentFragment();
  const re = /("[^"]*")|(#[^\n]*)|(\b\d+\b)|([A-Za-z_][A-Za-z0-9_]*)|([\s\S])/g;
  for (const m of source.matchAll(re)) {
    const [text, str, comment, number, word] = m;
    const cls = str ? "s" : comment ? "c" : number ? "n" : word && KEYWORDS.test(word) ? "k" : null;
    out.append(cls ? h("span." + cls, { text }) : document.createTextNode(text));
  }
  return out;
}

export const code = (source) => {
  const pre = h("pre.code");
  pre.append(tint(source));
  return pre;
};
const chip = (text) => h("span.chip", { text });

function plain(value) {
  return typeof value === "string" ? value : JSON.stringify(value, null, 2);
}

// -- exchange ----------------------------------------------------------- //
function scores(response) {
  const entries = Object.entries(response.scores || {}).sort((a, b) => b[1] - a[1]);
  if (!entries.length) return null;
  return h("div.block", {}, h("h4", { text: "Jev probabilities" }), entries.map(([name, p], i) =>
    h("div.score" + (i === 0 ? ".top" : ""), {}, h("span.name", { text: name }), h("div.bar", {}, h("i", { style: `width:${p * 100}%` })), h("span.small", { text: Math.round(p * 100) + "%" }))));
}

function section(title, value, open = true) {
  if (value === undefined || value === null || value === "") return null;
  const body = typeof value === "string" ? h("pre.code", { text: value }) : h("pre.code", { text: plain(value) });
  return h("details.block", { open }, h("summary", { text: title }), body);
}

export async function openExchange(line) {
  show("Loading...", "");
  let x;
  try {
    x = await getJSON(`/api/exchange/${line}`);
  } catch (error) {
    show("No exchange", String(error.message));
    return;
  }
  const req = x.request || {}, res = x.response || {};
  const title = { proposal: "Goal proposal", router: "Jev routing", writer: "Skill writer", plan: "Planner", command_map: "Command mapping" }[x.kind] || x.kind;
  const parsed = res.parsed && typeof res.parsed === "object" ? res.parsed : null;
  const thinking = x.kind === "plan" && parsed
    ? h("div.block", {}, h("h4", { text: "Thought" }), h("p.thought-read", { text: parsed.thought || "" }),
      (parsed.goals || []).map((g, i) => h("div.practice", {}, h("span.muted", { text: `${i + 1}` }), h("span", { text: g.text }),
        h("code", { text: g.condition || "(skill-verified)" }), g.skill ? h("span.muted", { text: g.skill }) : null)))
    : null;
  const meta = h("div.meta", {},
    chip(`${req.provider || res.provider || ""} / ${res.model || req.model || ""}`),
    chip(`${num(x.input_tokens)} in, ${num(x.output_tokens)} out`),
    chip(money(x.cost)),
    chip(x.latency_s === null ? "latency unknown" : x.latency_s.toFixed(1) + " s"),
    req.purpose ? chip(req.purpose) : null,
    x.decision ? chip("decision: " + x.decision.decision + (x.decision.skill ? " " + x.decision.skill : "")) : null,
    x.decision && x.decision.source ? chip("source: " + x.decision.source) : null,
    x.decision && x.decision.hint ? chip("hint: " + x.decision.hint) : null,
    res.after_death ? chip("after a death") : null);
  show(title, `evidence line ${line}`, meta,
    thinking,
    scores(res),
    section("System prompt", req.system, false),
    section("Briefing", req.briefing),
    section("Task", req.task),
    section("State", req.state, false),
    section("Questions", req.questions, false),
    section("Response", res.parsed !== undefined ? res.parsed : res.scores ? null : res),
    res.error ? section("Error", res.error) : null);
}

// -- skills ------------------------------------------------------------- //
export function practices(report) {
  if (!report) return null;
  const rows = [];
  report.attempts.forEach((attempt) => {
    attempt.practices.forEach((p) => rows.push(h("div.practice", {}, icon(p.passed ? "check" : "cross", p.passed ? "ok" : "no"),
      h("span", { text: `seed ${p.seed}` }), h("span.muted", { text: `attempt ${attempt.attempt}` }), h("span", { text: p.outcome }),
      p.ticks ? h("span.muted", { text: p.ticks + " ticks" }) : null)));
    if (!attempt.practices.length) rows.push(h("div.practice", {}, icon("cross", "no"), h("span", { text: `attempt ${attempt.attempt}: ${attempt.failure_kind || "no practice"}` })));
  });
  return h("div.block", {}, h("h4", { text: "Admission practice" }), rows);
}

export function openSkill(skill) {
  show(skill.name, skill.doc, h("div.meta", {}, chip(`${skill.rules} rules`), chip(skill.params.length ? `params: ${skill.params.join(", ")}` : "no params"), skill.report ? chip("written by the LLM") : chip("core")),
    h("div.block", {}, h("h4", { text: "Rule text" }), code(skill.source)), practices(skill.report));
}

export function openCandidate(report) {
  const last = report.attempts[report.attempts.length - 1];
  show(report.skill, report.reason || report.status,
    h("div.meta", {}, chip(report.status), chip(`${report.attempts.length} attempts`), chip(`${report.repairs_used ?? 0} repairs`)),
    last && last.source ? h("div.block", {}, h("h4", { text: "Latest candidate" }), code(last.source)) : null,
    practices(report),
    last && last.attempt !== undefined && report.attempts.flatMap((a) => a.practices).find((p) => !p.passed && p.digest)
      ? section("Failure digest", report.attempts.flatMap((a) => a.practices).find((p) => !p.passed && p.digest).digest, false) : null);
}
