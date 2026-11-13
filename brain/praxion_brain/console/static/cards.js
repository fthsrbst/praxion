// The timeline model of the live feed: evidence lines grouped into items.
//
// Pure data, no DOM, so the grouping is the same for the history the page
// loads and for the lines the stream appends. One goal reads as ONE card:
// the thought or plan that chose it, the route that picked its skill, the
// command, and the outcome. Operator lines and the bot's spoken lines are
// separate items so they read as a conversation between the goal cards.
import { epoch, failureWords, words, sentence } from "./util.js";

/** Filter categories; an item belongs to one or more. */
export const CATEGORIES = ["decisions", "goals", "talk", "problems"];

/** Cap on kept items: the page never grows without bound during a long run. */
export const MAX_ITEMS = 400;

const squash = (text) => String(text || "").replace(/\s+/g, "");

/** Kinds that become a marker: title, sub text, tone, categories, icon. */
const MARKERS = {
  died: () => ({ title: "The bot died", sub: "", tone: "bad", cats: ["problems", "goals"], icon: "skull", flavor: "death" }),
  survival_interrupt: (e) => ({ title: "Survival interrupt", sub: e.reason || e.skill || "", tone: "warn", cats: ["problems", "decisions"], icon: "shield", flavor: "reflex" }),
  survival_backoff: (e) => ({ title: `Survival backs off ${words(e.skill)}`, sub: e.failure_kind ? failureWords(e.failure_kind) : "", tone: "warn", cats: ["problems"], icon: "shield", flavor: "reflex" }),
  night_wait: (e) => ({ title: "Waiting out the night", sub: e.reason || "", tone: "", cats: ["decisions"], icon: "moon", flavor: "reflex" }),
  nothing_to_do: (e) => ({ title: "Nothing to do", sub: [words(e.reason), e.detail].filter(Boolean).join(" - "), tone: "", cats: ["decisions"], icon: "pause", flavor: "system", minor: true }),
  pick_gap: (e) => ({ title: `Waiting ${Math.round((e.ticks || 0) / 20)} s after a failure`, sub: failureWords(e.failure_kind), tone: "", cats: ["problems"], icon: "clock", flavor: "system", minor: true }),
  governor_denied: (e) => ({ title: `Spending guard denied ${words(e.call)}`, sub: words(e.reason), tone: "warn", cats: ["problems"], icon: "alert", flavor: "system" }),
  curriculum_drift: (e) => ({ title: "Not making progress", sub: `${e.goals_without_progress} goals without a new rung`, tone: "warn", cats: ["problems"], icon: "alert", flavor: "system" }),
  drift_brake: (e) => ({ title: "Drift brake", sub: e.reason || "", tone: "warn", cats: ["problems"], icon: "alert", flavor: "system" }),
  loop_error: (e) => ({ title: "Loop error", sub: e.error || "", tone: "bad", cats: ["problems"], icon: "alert", flavor: "system" }),
  wiki_error: (e) => ({ title: "Wiki lookup failed", sub: e.query || e.error || "", tone: "warn", cats: ["problems"], icon: "alert", flavor: "system" }),
  plan_invalid: (e) => ({ title: "Plan rejected", sub: words(e.reason), tone: "warn", cats: ["problems", "decisions"], icon: "cross", flavor: "system" }),
  proposal_invalid: (e) => ({ title: "Proposal rejected", sub: words(e.reason), tone: "warn", cats: ["problems", "decisions"], icon: "cross", flavor: "system" }),
  loop_started: () => ({ title: "Run started", sub: "", tone: "", cats: ["decisions"], icon: "bolt", flavor: "system", minor: true }),
  loop_stopped: () => ({ title: "Run stopped", sub: "", tone: "", cats: ["decisions"], icon: "stop", flavor: "system", minor: true }),
  loop_disconnected: () => ({ title: "Body disconnected", sub: "", tone: "warn", cats: ["problems"], icon: "alert", flavor: "system" }),
  loop_reconnect: () => ({ title: "Reconnected to the body", sub: "", tone: "", cats: ["decisions"], icon: "bolt", flavor: "system", minor: true }),
  write_started: (e) => ({ title: `Writing a new skill: ${words(e.skill)}`, sub: "", tone: "", cats: ["decisions"], icon: "pen", flavor: "skill" }),
  writer_response: (e) => ({ title: "Skill drafted", sub: e.parsed && e.parsed.rationale || "", tone: "", cats: ["decisions"], icon: "pen", flavor: "skill", exchange: true }),
  admission_attempt: (e) => ({ title: `Skill admission attempt ${(e.attempt ?? 0) + 1}`, sub: e.model || "", tone: "", cats: ["decisions"], icon: "pen", flavor: "skill", exchange: true, minor: true }),
  admission_admitted: (e) => ({ title: `Learned ${words(e.skill)}`, sub: e.repairs ? `${e.repairs} repairs` : "", tone: "good", cats: ["decisions", "goals"], icon: "spark", flavor: "skill" }),
  admission_report: (e) => {
    const r = e.report || {};
    const ok = r.status === "admitted";
    return { title: `Skill ${ok ? "admitted" : "not admitted"}: ${words(r.skill_name)}`, sub: ok ? "" : words(r.reason), tone: ok ? "good" : "bad", cats: ok ? ["decisions"] : ["decisions", "problems"], icon: "box", flavor: "skill" };
  },
};

/** The control words an operator can say; they read as a small command, not a task. */
const CONTROL_ICON = { pause: "pause", resume: "play", stop_goal: "stop" };

export function createTimeline() {
  const items = [];
  const open = new Map(); // "goal_id" -> goal card still running
  const topics = new Map(); // narrate line_id -> topic
  let plan = null; // the newest plan item: its steps are still being worked through
  let pending = {}; // what the next command_sent inherits

  function push(item, changes) {
    items.push(item);
    changes.push({ item, isNew: true });
    if (items.length > MAX_ITEMS) {
      const gone = items.splice(0, items.length - MAX_ITEMS);
      for (const old of gone) {
        if (old.type === "goal" && open.get(old.goalId) === old) open.delete(old.goalId);
        if (plan === old) plan = null;
      }
    }
    return item;
  }

  function touch(item, changes) {
    if (!changes.some((c) => c.item === item)) changes.push({ item, isNew: false });
  }

  function planStep(condition, skill) {
    if (!plan) return null;
    const queued = plan.steps.filter((step) => step.status === "queued");
    return (
      queued.find((step) => squash(step.condition) === squash(condition) && (!skill || !step.skill || step.skill === skill))
      || queued.find((step) => skill && step.skill === skill)
      || queued.find((step) => squash(step.condition) === squash(condition))
      || null
    );
  }

  function makePlan(e, thought, goals, tag) {
    return {
      id: "p:" + e.n,
      type: "plan",
      n: e.n,
      ts: epoch(e.timestamp),
      cats: ["decisions"],
      tone: "",
      tag,
      thought: thought || "",
      steps: goals.map((goal, index) => ({
        index,
        text: goal.text || "",
        condition: goal.condition || "",
        skill: goal.skill || null,
        rationale: goal.rationale || "",
        status: "queued",
        reason: null,
        cardId: null,
      })),
      exchangeN: e.n,
    };
  }

  function ingest(e) {
    const changes = [];
    const ts = epoch(e.timestamp);
    switch (e.kind) {
      case "plan": {
        // The `plan` line follows its `plan_response`; the card reads the same
        // exchange from either, so the line number of the plan opens it.
        plan = push(makePlan(e, e.thought, e.goals || [], "Plan"), changes);
        break;
      }
      case "proposal_response": {
        const parsed = e.parsed;
        if (!parsed) break;
        plan = push(makePlan(e, parsed.rationale, [parsed], "Proposal"), changes);
        plan.thought = "";
        break;
      }
      case "plan_step_skipped": {
        const step = e.step || {};
        const target = plan && plan.steps.find((s) => s.status === "queued" && squash(s.condition) === squash(step.condition) && squash(s.text) === squash(step.text));
        const found = target || (plan && plan.steps.find((s) => s.status === "queued" && squash(s.condition) === squash(step.condition)));
        if (found) {
          found.status = "skipped";
          found.reason = e.reason || "skipped";
          touch(plan, changes);
        }
        push({
          id: "n:" + e.n, type: "marker", n: e.n, ts, flavor: "system", icon: "cross", tone: "warn", cats: ["problems", "decisions"], minor: true,
          title: `Skipped: ${sentence(step.text || step.condition)}`, sub: words(e.reason) + (e.detail ? ` - ${e.detail}` : ""),
        }, changes);
        break;
      }
      case "plan_dropped": {
        if (plan) {
          for (const step of plan.steps) if (step.status === "queued") { step.status = "skipped"; step.reason = e.reason || "dropped"; }
          touch(plan, changes);
        }
        push({ id: "n:" + e.n, type: "marker", n: e.n, ts, flavor: "system", icon: "cross", tone: "warn", cats: ["problems", "decisions"], title: "Plan dropped", sub: words(e.reason) }, changes);
        break;
      }
      case "router_decision":
      case "loop_route": {
        if (e.kind === "loop_route" && pending.route && e.n - pending.route.n <= 3) break;
        const scores = e.scores || {};
        const top = Object.entries(scores).sort((a, b) => b[1] - a[1])[0];
        pending.route = { n: e.n, decision: e.decision, skill: e.skill, hint: e.hint || null, source: e.source || null, top: top ? { name: top[0], score: top[1] } : null, scores };
        break;
      }
      case "user_goal_result": {
        const p = e.proposal || {};
        const v = e.validation || {};
        pending.user = { text: p.text, ok: v.ok !== false, reason: v.reason };
        if (v.ok === false) {
          push({ id: "n:" + e.n, type: "marker", n: e.n, ts, flavor: "system", icon: "user", tone: "warn", cats: ["talk", "problems"], title: "Your goal was rejected", sub: `${p.text || ""} - ${words(v.reason)}` }, changes);
        }
        break;
      }
      case "survival_goal":
        pending.survival = { kind: e.survival, skill: e.skill, reason: e.reason };
        break;
      case "code_only_goal":
        pending.codeOnly = { reason: e.reason, text: e.goal && e.goal.text };
        break;
      case "command_sent": {
        const step = planStep(e.condition, e.skill);
        const survival = pending.survival && pending.survival.skill === e.skill ? pending.survival : null;
        let origin = "plan";
        let text = step ? step.text : "";
        if (survival) { origin = "survival"; text = text || `${sentence(words(survival.kind))}: ${words(survival.skill)}`; }
        else if (pending.user) { origin = "you"; text = pending.user.text || text; }
        else if (pending.codeOnly) { origin = "fallback"; text = pending.codeOnly.text || text; }
        else if (!step) { origin = plan && plan.tag === "Proposal" ? "plan" : "direct"; }
        if (!text) text = words(e.skill) || e.condition || "goal";
        const card = {
          id: "g:" + e.goal_id + ":" + e.command_id,
          type: "goal",
          n: e.n,
          ts,
          t0: ts,
          t1: null,
          goalId: e.goal_id,
          commandId: e.command_id,
          skill: e.skill,
          condition: e.condition,
          text,
          origin,
          why: survival ? survival.reason : pending.codeOnly ? words(pending.codeOnly.reason) : step ? step.rationale : "",
          stepNo: step && plan ? step.index + 1 : null,
          stepCount: step && plan ? plan.steps.length : null,
          planN: step && plan ? plan.n : null,
          route: pending.route || null,
          status: "running",
          failureKind: null,
          ticks: null,
          rungs: null,
          cats: ["goals"],
          tone: "",
        };
        if (step) {
          step.status = "playing";
          step.cardId = card.id;
          touch(plan, changes);
        }
        pending = {};
        open.set(e.goal_id, card);
        push(card, changes);
        break;
      }
      case "goal_finished": {
        let card = open.get(e.goal_id);
        if (!card) {
          // The history window began mid-goal: show what the outcome alone says.
          card = push({
            id: "g:" + e.goal_id + ":" + e.command_id, type: "goal", n: e.n, ts, t0: null, goalId: e.goal_id, commandId: e.command_id,
            skill: e.skill, condition: "", text: words(e.skill) || "goal", origin: "direct", why: "", stepNo: null, stepCount: null, planN: null, route: null, cats: ["goals"], tone: "",
          }, changes);
        }
        open.delete(e.goal_id);
        card.status = e.status || "done";
        card.t1 = ts;
        card.failureKind = e.failure_kind || null;
        card.ticks = e.ticks ?? null;
        card.rungs = e.rungs_met ?? null;
        card.detail = e.detail || null;
        card.tone = card.status === "done" ? "good" : card.status === "failed" ? "bad" : "warn";
        if (card.status === "failed" && !card.cats.includes("problems")) card.cats.push("problems");
        const step = plan && plan.steps.find((s) => s.cardId === card.id);
        if (step) {
          step.status = card.status === "done" ? "done" : card.status === "failed" ? "failed" : "interrupted";
          touch(plan, changes);
        }
        touch(card, changes);
        break;
      }
      case "goal_preempted": {
        const card = open.get(e.goal_id);
        if (card) { card.preemptedBy = e.by; touch(card, changes); }
        break;
      }
      case "chat_received": {
        const control = e.control;
        push({
          id: "n:" + e.n, type: "you", n: e.n, ts, source: "chat", text: e.text || "", control: control || null,
          result: null, cats: ["talk"], tone: "",
        }, changes);
        break;
      }
      case "command_received": {
        // Chat already produced a `chat_received` line with the same text.
        const recent = items.slice(-8).some((i) => i.type === "you" && squash(i.text) === squash(e.text));
        if (!recent) {
          push({ id: "n:" + e.n, type: "you", n: e.n, ts, source: e.source || "console", text: e.text || "", control: null, result: null, cats: ["talk"], tone: "" }, changes);
        }
        break;
      }
      case "console_control": {
        if (e.action === "task" || e.action === "new_goal") {
          const text = [e.text, e.condition].filter(Boolean).join("  ");
          const result = e.result && e.result !== "queued" ? words(e.result) : null;
          const recent = items.slice(-8).find((i) => i.type === "you" && squash(i.text) === squash(e.text));
          if (recent) { recent.result = recent.result || result; touch(recent, changes); }
          else push({ id: "n:" + e.n, type: "you", n: e.n, ts, source: "console", text, control: null, result, cats: ["talk"], tone: "" }, changes);
        } else {
          push({ id: "n:" + e.n, type: "marker", n: e.n, ts, flavor: "you", icon: CONTROL_ICON[e.action] || "user", tone: "", cats: ["talk"], title: `You: ${words(e.action)}`, sub: e.result && e.result !== "ok" ? words(e.result) : "", minor: true }, changes);
        }
        break;
      }
      case "command_resolved": {
        const target = [...items].reverse().slice(0, 20).find((i) => i.type === "you");
        if (target) {
          target.result = e.resolution === "goal"
            ? `understood as ${e.skill ? words(e.skill) : e.condition || "a goal"}`
            : e.resolution === "unsupported" ? "cannot do that" : words(e.resolution) + (e.reason ? `: ${words(e.reason)}` : "");
          target.resolution = e.resolution;
          touch(target, changes);
        }
        break;
      }
      case "narrate":
        if (e.line_id) topics.set(e.line_id, e.topic || "");
        break;
      case "narrate_sent": {
        push({ id: "n:" + e.n, type: "say", n: e.n, ts, text: e.text || "", topic: topics.get(e.line_id) || "", cats: ["talk"], tone: "" }, changes);
        topics.delete(e.line_id);
        break;
      }
      default: {
        const make = MARKERS[e.kind];
        if (!make) break;
        const spec = make(e);
        push({ id: "n:" + e.n, type: "marker", n: e.n, ts, ...spec }, changes);
      }
    }
    return changes;
  }

  return { ingest, items, get plan() { return plan; } };
}

/** The outcome of a goal card as a label and, when it failed or stopped, the reason in plain words. */
export function outcome(card) {
  switch (card.status) {
    case "running": return { label: "Playing", detail: "" };
    case "done": return { label: "Done", detail: "" };
    case "failed": return { label: "Failed", detail: failureWords(card.failureKind) || "no reason recorded" };
    case "interrupted": {
      const by = card.failureKind === "preempted_by_survival" ? "survival took over"
        : card.preemptedBy ? `${card.preemptedBy} took over` : "stopped before it finished";
      return { label: "Interrupted", detail: by };
    }
    default: return { label: sentence(card.status), detail: "" };
  }
}
