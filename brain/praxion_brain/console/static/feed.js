// The live feed: evidence lines turned into a timeline of decisions.
import { h, icon, el, clock, sentence } from "./util.js";

/** Kinds worth showing as decisions; the rest only appear under "Everything". */
const MAJOR = new Set([
  "proposal_response", "proposal_invalid", "loop_route", "code_only_goal", "command_sent", "goal_finished",
  "admission_attempt", "admission_admitted", "admission_report", "console_control", "user_goal_result",
  "governor_denied", "loop_error", "pick_gap", "curriculum_drift", "loop_started", "loop_stopped", "writer_response",
]);
const HIDDEN = new Set(["heartbeat_requested", "heartbeat_quiet", "proposal_request", "router_request", "router_response", "router_decision", "proposal_result", "writer_request", "command_accepted", "route_code_only"]);
const MAX_ITEMS = 400;

/** Describe one entry: icon, title, subtitle, tone, and whether it opens an exchange. */
export function describe(e) {
  const pct = (v) => Math.round(v * 100) + "%";
  switch (e.kind) {
    case "proposal_response":
      return { icon: "brain", title: "Proposed: " + sentence(e.parsed && e.parsed.text), sub: e.parsed ? `${e.parsed.condition} - ${e.parsed.rationale}` : "", exchange: true };
    case "proposal_invalid":
      return { icon: "cross", title: "Proposal rejected", sub: e.reason, tone: "warn" };
    case "loop_route": {
      const top = e.scores && Object.entries(e.scores).sort((a, b) => b[1] - a[1])[0];
      return { icon: "route", title: e.decision === "play" ? `Routed to ${e.skill}` : `Route: ${e.decision} a new skill`, sub: top ? `Jev: ${top[0]} ${pct(top[1])}` : e.condition, exchange: !!e.scores };
    }
    case "code_only_goal":
      return { icon: "flag", title: "Code-only goal: " + sentence(e.goal && e.goal.text), sub: `${e.skill} (${e.reason})`, tone: "warn" };
    case "command_sent":
      return { icon: "play", title: `Sent ${e.skill}`, sub: e.condition };
    case "goal_finished": {
      const ok = e.status === "done";
      return { icon: ok ? "check" : e.status === "interrupted" ? "stop" : "cross", title: `${sentence(e.status)}: ${e.skill || e.goal_id}`, sub: [e.failure_kind, e.ticks != null ? e.ticks + " ticks" : null, `${e.rungs_met} rungs`].filter(Boolean).join(" - "), tone: ok ? "" : e.status === "failed" ? "bad" : "warn" };
    }
    case "writer_response":
      return { icon: "pen", title: "Skill drafted", sub: e.parsed && e.parsed.rationale, exchange: true };
    case "admission_attempt":
      return { icon: "pen", title: `Admission attempt ${e.attempt}`, sub: e.model, exchange: true };
    case "admission_admitted":
      return { icon: "spark", title: `Admitted ${e.skill}`, sub: `${e.repairs} repairs` };
    case "admission_report":
      return { icon: "box", title: `Admission ${e.report && e.report.status}: ${e.report && e.report.skill_name}`, sub: "", tone: e.report && e.report.status === "admitted" ? "" : "bad" };
    case "console_control":
      return { icon: "user", title: `You: ${e.action.replace("_", " ")}`, sub: [e.text, e.condition, e.result].filter(Boolean).join(" - "), tone: "user" };
    case "user_goal_result":
      return { icon: "user", title: "Your goal: " + sentence(e.proposal && e.proposal.text), sub: e.validation && (e.validation.ok ? "accepted" : "rejected: " + e.validation.reason), tone: e.validation && e.validation.ok ? "user" : "warn" };
    case "governor_denied":
      return { icon: "alert", title: `Governor denied ${e.call}`, sub: e.reason, tone: "warn" };
    case "pick_gap":
      return { icon: "pause", title: `Waiting ${e.ticks} ticks after a failure`, sub: e.failure_kind };
    case "curriculum_drift":
      return { icon: "alert", title: "Curriculum drift", sub: `${e.goals_without_progress} goals without progress`, tone: "warn" };
    case "loop_error":
      return { icon: "alert", title: "Loop error", sub: e.error, tone: "bad" };
    case "loop_transition":
      return { icon: "dot", title: `${e.from_state} to ${e.to_state}`, sub: e.reason, minor: true };
    default:
      return { icon: "dot", title: e.kind.replace(/_/g, " "), sub: "", minor: true };
  }
}

export function createFeed(open) {
  const root = el("feed");
  let everything = false;
  let lastN = 0;
  const list = h("ol.timeline", { "aria-live": "polite" });
  const toggle = h("button.toggle", { type: "button", "aria-pressed": "false", onclick: () => {
    everything = !everything;
    toggle.setAttribute("aria-pressed", String(everything));
    for (const item of list.children) item.hidden = !visible(item.dataset.kind);
  } }, "Everything");
  root.replaceChildren(h("h2", {}, "Live feed", h("span.feed-tools", {}, toggle)), list);

  const visible = (kind) => !HIDDEN.has(kind) && (everything || MAJOR.has(kind));

  function add(e, animate = true) {
    if (e.n <= lastN) return;
    lastN = e.n;
    const d = describe(e);
    const item = h("li.item" + (d.exchange ? ".click" : "") + (d.minor ? ".minor" : "") + (d.tone ? "." + d.tone : ""), {
      "data-kind": e.kind, "data-n": e.n, tabindex: d.exchange ? "0" : null, role: d.exchange ? "button" : null,
      style: animate ? null : "animation:none",
    }, h("span.ico", {}, icon(d.icon)), h("div", {}, h("div.title", { text: d.title }), d.sub ? h("div.sub", { text: d.sub }) : null), h("span.when", { text: clock(e.timestamp) }));
    if (d.exchange) {
      const go = () => open("exchange", e.n);
      item.addEventListener("click", go);
      item.addEventListener("keydown", (k) => { if (k.key === "Enter" || k.key === " ") { k.preventDefault(); go(); } });
    }
    item.hidden = !visible(e.kind);
    const pinned = list.scrollHeight - list.scrollTop - list.clientHeight < 80;
    list.append(item);
    while (list.children.length > MAX_ITEMS) list.firstChild.remove();
    if (pinned) list.scrollTop = list.scrollHeight;
  }

  return {
    add,
    load(lines) { lines.forEach((e) => add(e, false)); list.scrollTop = list.scrollHeight; },
    get last() { return lastN; },
  };
}
