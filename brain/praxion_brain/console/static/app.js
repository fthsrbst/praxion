// Praxion console: wires the API, the live stream and the panels together.
import { getJSON, control, stream } from "./api.js";
import { el } from "./util.js";
import { renderHero, renderSpend, renderLadder, renderMetrics, renderSkills } from "./panels.js";
import { createFeed } from "./feed.js";
import { openExchange, openSkill, openCandidate, closeDrawer } from "./inspector.js";

const opener = (kind, value) => ({ exchange: openExchange, skill: openSkill, candidate: openCandidate })[kind](value);
const feed = createFeed(opener);
let current = {};

function onState(s) {
  current = s;
  el("world").textContent = s.world_id ? `world ${s.world_id}` : "";
  renderHero(s);
  renderSpend(s);
  renderLadder(s);
  const pause = el("pause-btn");
  pause.textContent = s.paused ? "Resume" : "Pause";
  el("stop-btn").disabled = !s.goal;
}

function setLink(on) {
  const link = el("link");
  link.className = "link " + (on ? "on" : "off");
  link.lastElementChild.textContent = on ? "live" : "reconnecting";
}

async function refresh(path, render) {
  try { render(await getJSON(path)); } catch { /* the link chip shows trouble; try again next round */ }
}

// -- command dock --------------------------------------------------------- //
const note = el("dock-note");
function say(text, error = false) {
  note.textContent = text;
  note.classList.toggle("err", error);
}

async function send(body, done) {
  try {
    await control(body);
    say(done);
  } catch (error) {
    say(error.message, true);
  }
}

el("dock").addEventListener("submit", async (event) => {
  event.preventDefault();
  const text = el("goal-text").value.trim();
  const condition = el("goal-cond").value.trim();
  if (!condition) { say("Add a condition, e.g. has(furnace) or count(cobblestone) >= 16.", true); return; }
  await send({ action: "new_goal", text, condition }, "Queued: it goes before the next pick.");
  if (!note.classList.contains("err")) { el("goal-text").value = ""; el("goal-cond").value = ""; }
});

el("pause-btn").addEventListener("click", () =>
  send({ action: current.paused ? "resume" : "pause" }, current.paused ? "Resuming." : "Paused: the current goal continues, no new picks."));

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

// -- start ------------------------------------------------------------------ //
async function start() {
  const history = await getJSON("/api/evidence?limit=600").catch(() => ({ lines: [], last: 0 }));
  feed.load(history.lines);
  onState(await getJSON("/api/state").catch(() => ({})));
  stream(history.last, {
    evidence: (e) => feed.add(e),
    state: onState,
    link: setLink,
  });
  const loops = [["/api/metrics", renderMetrics, 10000], ["/api/skills", (d) => renderSkills(d, opener), 15000]];
  for (const [path, render, every] of loops) {
    refresh(path, render);
    setInterval(() => refresh(path, render), every);
  }
}
start();
