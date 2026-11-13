// Praxion console: the shell. It owns the stream, the store, the session bar,
// the command dock and the page router; each page lives in pages/*.js and is
// built the first time it is opened.
import { getJSON, control, sessionAction, stream } from "./api.js";
import { el, words } from "./util.js";
import { store, onEvidence, setState, addEvidence, loadEvidence, setLink as storeLink, onState as onStoreState } from "./store.js";
import { openExchange, openSkill, openCandidate, closeDrawer } from "./inspector.js";

const open = (kind, value) => ({ exchange: openExchange, skill: openSkill, candidate: openCandidate })[kind](value);
const ctx = { open };
let current = {};

const SERVER_LABEL = { idle: "Idle", starting: "Starting", running: "Ready", stopping: "Stopping", failed: "Failed" };
const BOT_LABEL = { idle: "Idle", starting: "Starting", running: "Running", stopping: "Stopping", failed: "Failed" };

function styleChip(node, phase, live) {
  node.className = "chip" + (live ? " live" : phase === "failed" ? " paused" : "");
}

// A managed console owns one server and one bot on it. A brain-only console
// (started with `praxion_brain run`) has no `session`, so this whole card stays
// hidden and the goal dock works exactly as before.
let runSession = null;
let runPending = new Set();
let runError = null;
// Bumped by every stream state; a reply older than the newest state loses.
let stateEpoch = 0;

function runText(session) {
  if (runError) return runError;
  if (!session) return "";
  const server = session.server || {};
  const bot = session.bot || {};
  if (server.error) return `Server: ${server.error}`;
  if (bot.error) return `Bot: ${bot.error}`;
  if (server.phase === "starting") return "Starting the Minecraft server\u2026";
  if (server.phase === "stopping") return "Stopping the server and its bot\u2026";
  if (server.ready && server.address) {
    if (bot.phase === "starting") return `Server ready on ${server.address}; starting the bot\u2026`;
    if (bot.ready) return `Live on ${server.address}; the bot is running.`;
    if (bot.phase === "stopping") return "Stopping the bot; the server stays up.";
    if (bot.phase === "failed") return "The bot failed; the server is still up. Retry Start bot.";
    return `Server ready on ${server.address}. Start the bot when you want it.`;
  }
  if (bot.phase === "starting") return "Starting the bot\u2026";
  return "No server. Start a local server, then the bot.";
}

function renderRun(session) {
  const root = el("run");
  if (!session || !session.managed) { root.hidden = true; return; }
  root.hidden = false;
  runSession = session;
  const server = session.server || { phase: "idle", ready: false, can: {} };
  const bot = session.bot || { phase: "idle", ready: false, can: {} };

  const serverChip = el("run-server");
  serverChip.textContent = `Server: ${SERVER_LABEL[server.phase] || server.phase}`;
  styleChip(serverChip, server.phase, !!server.ready);
  const botChip = el("run-bot");
  botChip.textContent = `Bot: ${BOT_LABEL[bot.phase] || bot.phase}`;
  styleChip(botChip, bot.phase, !!bot.ready);

  const status = el("run-status");
  status.textContent = runText(session);
  status.classList.toggle(
    "err",
    !!runError || !!server.error || !!bot.error
      || server.phase === "failed" || bot.phase === "failed",
  );

  const address = el("run-address");
  address.hidden = !(server.ready && server.address);
  address.textContent = server.address || "";

  const can = session.can || {};
  // Join only blocks another Join: the server may still be stopped while the
  // slow client launch runs. Every other action is exclusive with all of them.
  const joinBusy = runPending.has("join");
  const locked = [...runPending].some((action) => action !== "join");
  const controls = [
    ["run-start-server", "start_server", can.start_server],
    ["run-stop-server", "stop_server", can.stop_server],
    ["run-start-bot", "start_bot", can.start_bot],
    ["run-stop-bot", "stop_bot", can.stop_bot],
  ];
  for (const [id, action, allowed] of controls) {
    const node = el(id);
    node.disabled = locked || !allowed;
    node.setAttribute("aria-busy", runPending.has(action) ? "true" : "false");
  }
  const join = el("run-join");
  join.textContent = session.client && session.client.running ? "Client running" : "Join Minecraft";
  join.disabled = runPending.size > 0 || !can.join;
  join.setAttribute("aria-busy", joinBusy ? "true" : "false");
}

async function runAction(action) {
  runPending.add(action);
  runError = null;
  const epoch = stateEpoch;
  renderRun(runSession);
  try {
    const result = await sessionAction(action);
    // A Join can overlap Stop server, so its reply may describe a session that
    // the stream has already moved past, or one a second action is replacing.
    // Only a reply that is still the sole, newest view may be rendered.
    if (result.session && stateEpoch === epoch && runPending.size === 1) {
      renderRun(result.session);
    }
  } catch (error) {
    runError = error.message;
  } finally {
    runPending.delete(action);
    renderRun(runSession);
  }
}

el("run-start-server").addEventListener("click", () => runAction("start_server"));
el("run-stop-server").addEventListener("click", () => runAction("stop_server"));
el("run-start-bot").addEventListener("click", () => runAction("start_bot"));
el("run-stop-bot").addEventListener("click", () => runAction("stop_bot"));
el("run-join").addEventListener("click", () => runAction("join"));

function gateDock(session) {
  const managed = !!(session && session.managed);
  const live = !managed || !!(session.bot && session.bot.phase === "running");
  for (const id of ["goal-text", "goal-cond", "give", "pause-btn", "task-text", "task-send"]) el(id).disabled = !live;
  el("stop-btn").disabled = !live || !current.goal;
  el("dock").classList.toggle("locked", !live);
}

function onState(s) {
  current = s;
  stateEpoch += 1;
  const session = s.session || null;
  const server = session && session.server;
  renderRun(session);
  gateDock(session);
  const worldId = s.world_id || (server && server.world_id);
  el("world").textContent = worldId ? `world ${worldId}` : "";
  const pause = el("pause-btn");
  pause.textContent = s.paused ? "Resume" : "Pause";
}

function showLink(on) {
  const link = el("link");
  link.className = "link " + (on ? "on" : "off");
  link.lastElementChild.textContent = on ? "live" : "reconnecting";
}

// -- command dock ---------------------------------------------------------- //
const note = el("dock-note");
const reply = el("dock-reply");
function say(text, error = false) {
  note.textContent = text;
  note.classList.toggle("err", error);
}

async function send(body, done) {
  try {
    await control(body);
    say(done);
    return true;
  } catch (error) {
    say(error.message, true);
    return false;
  }
}

// The bot's answer to the last task: the loop writes it into the evidence
// (how it understood the words, or why it cannot), and this line shows it.
let awaiting = null;
function showReply(text) {
  reply.textContent = text;
  reply.hidden = !text;
}
onEvidence((entry) => {
  if (!awaiting) return;
  if (Date.now() - awaiting.at > 90000) { awaiting = null; return; }
  if (entry.kind === "command_resolved") {
    showReply(entry.resolution === "goal"
      ? `Understood as ${entry.skill ? words(entry.skill) : entry.condition || "a goal"}.`
      : entry.resolution === "unsupported" ? "Praxion cannot do that yet."
        : `${words(entry.resolution)}${entry.reason ? `: ${words(entry.reason)}` : ""}.`);
  } else if (entry.kind === "console_control" && (entry.action === "task" || entry.action === "new_goal") && entry.result && !["queued", "ok"].includes(entry.result)) {
    showReply(words(entry.result));
  } else if (entry.kind === "narrate_sent" && entry.text) {
    showReply(`Praxion: ${entry.text}`);
  }
});

const taskText = el("task-text");
let pendingVoice = null;
function cancelVoice(message) {
  if (!pendingVoice) return;
  clearTimeout(pendingVoice.timer);
  pendingVoice = null;
  if (message) say(message);
}

async function sendTask() {
  const text = taskText.value.trim();
  if (!text) { say("Type what Praxion should do.", true); return; }
  cancelVoice();
  showReply("");
  awaiting = { at: Date.now() };
  const ok = await send({ action: "task", text }, "Sent: Praxion will answer here.");
  if (ok) taskText.value = ""; else awaiting = null;
}
el("task-form").addEventListener("submit", (event) => { event.preventDefault(); sendTask(); });

// A voice transcript lands in the task box and is sent after a short pause,
// unless the operator edits the words or presses Esc.
const VOICE_SEND_MS = 2500;
document.addEventListener("praxion:voice-task", (event) => {
  if (el("task-text").disabled) return; // no live bot: let voice.js say so itself
  event.preventDefault();
  cancelVoice();
  taskText.value = event.detail.text;
  say("Heard you. Sending in a moment: edit the words or press Esc to cancel.");
  pendingVoice = { timer: setTimeout(() => { pendingVoice = null; sendTask(); }, VOICE_SEND_MS) };
});
taskText.addEventListener("input", () => cancelVoice("Voice task paused: press Send when ready."));
document.addEventListener("keydown", (event) => {
  if (event.key === "Escape" && pendingVoice) { cancelVoice("Cancelled."); taskText.value = ""; }
});

// Advanced: the explicit goal-with-condition form.
const advanced = el("adv-btn");
advanced.addEventListener("click", () => {
  const open = el("goal-form").hidden;
  el("goal-form").hidden = !open;
  advanced.setAttribute("aria-expanded", String(open));
  advanced.classList.toggle("on", open);
  if (open) el("goal-cond").focus();
});

el("goal-form").addEventListener("submit", async (event) => {
  event.preventDefault();
  const text = el("goal-text").value.trim();
  const condition = el("goal-cond").value.trim();
  if (!condition) { say("Add a condition, e.g. has(furnace) or count(cobblestone) >= 16.", true); return; }
  await send({ action: "new_goal", text, condition }, "Queued: it goes before the next pick.");
  if (!note.classList.contains("err")) { el("goal-text").value = ""; el("goal-cond").value = ""; }
});

el("pause-btn").addEventListener("click", () =>
  send({ action: current.paused ? "resume" : "pause" }, current.paused ? "Resuming." : "Paused: the current goal continues, no new picks."));

// The dock floats over the page: keep the last card scrollable clear of it.
const dockEl = el("dock");
function reserveDock() {
  document.body.style.paddingBottom = `${Math.ceil(dockEl.getBoundingClientRect().height) + 48}px`;
}
new ResizeObserver(reserveDock).observe(dockEl);
reserveDock();

let armed = null;
const stop = el("stop-btn");
stop.addEventListener("click", () => {
  if (!armed) {
    stop.textContent = "Confirm stop";
    stop.classList.add("confirm");
    say("Click again to stop the running goal.");
    armed = setTimeout(disarm, 4000);
    return;
  }
  disarm();
  send({ action: "stop_goal" }, "Stop requested.");
});
function disarm() {
  clearTimeout(armed);
  armed = null;
  stop.textContent = "Stop goal";
  stop.classList.remove("confirm");
}

// -- drawer ----------------------------------------------------------------- //
el("scrim").addEventListener("click", closeDrawer);
document.addEventListener("keydown", (event) => { if (event.key === "Escape" && !el("drawer").hidden) closeDrawer(); });

// -- pages ------------------------------------------------------------------ //
const PAGES = {
  live: () => import("./pages/live.js"),
  mind: () => import("./pages/mind.js"),
  run: () => import("./pages/run.js"),
  skills: () => import("./pages/skills.js"),
  wiki: () => import("./pages/wiki.js"),
};
const mounted = new Map();
let shown = null;

async function showPage(name) {
  if (!PAGES[name]) name = "live";
  for (const link of document.querySelectorAll("nav.tabs a")) {
    const on = link.dataset.page === name;
    link.classList.toggle("on", on);
    if (on) link.setAttribute("aria-current", "page"); else link.removeAttribute("aria-current");
  }
  for (const section of document.querySelectorAll("main > .page")) section.hidden = section.id !== `page-${name}`;
  if (shown && mounted.get(shown)) mounted.get(shown).hide?.();
  shown = name;
  try {
    if (!mounted.has(name)) mounted.set(name, (await PAGES[name]()).mount(ctx));
    if (shown === name) mounted.get(name).show?.();
  } catch (error) {
    const section = el(`page-${name}`);
    section.replaceChildren(Object.assign(document.createElement("p"), { className: "page-note", textContent: `This page failed to load: ${error.message}` }));
  }
}

const pageFromHash = () => location.hash.replace(/^#\/?/, "") || "live";
window.addEventListener("hashchange", () => showPage(pageFromHash()));

// -- start ------------------------------------------------------------------ //
async function start() {
  const history = await getJSON("/api/evidence?limit=600").catch(() => ({ lines: [], last: 0 }));
  loadEvidence(history.lines);
  setState(await getJSON("/api/state").catch(() => ({})));
  onStoreState(onState);
  onState(store.state);
  await showPage(pageFromHash());
  stream(store.last, {
    evidence: (e) => addEvidence(e),
    state: setState,
    link: (on) => { storeLink(on); showLink(on); },
  });
}
start();
