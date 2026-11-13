// Voice: speak the bot's narration lines and transcribe held-key speech.
//
// Self-contained on purpose (nothing else imports it, it imports nothing from
// the console): one script tag in index.html starts it. The panel is fixed in
// a corner and is replaced by the console redesign; the two API shapes it uses
// are the voice contract of 2026-11-12.

const POLL_MS = 1000;
//: A line that waited this long in the queue is not worth saying any more.
const STALE_MS = 15000;
//: A tap is not speech; shorter clips are dropped without a request.
const MIN_RECORD_MS = 300;
const SOUND_KEY = "praxion.voice.sound";
const MIME_TYPES = [
  "audio/webm;codecs=opus",
  "audio/webm",
  "audio/mp4",
  "audio/ogg;codecs=opus",
];

const state = {
  enabled: false,
  sound: readPref(),
  cursor: null,
  initialized: false,
  polling: false,
  queue: [],
  busy: false,
  blocked: false,
  talking: false,
  active: null,
  stream: null,
  permission: "unknown",
};

const player = new Audio();
let playerUrl = null;
let finishClip = null;

const root = document.createElement("section");
root.id = "voice";
root.className = "voice";
root.dataset.state = "off";
root.setAttribute("aria-label", "Voice");
root.innerHTML = `
  <button id="voice-talk" class="voice-btn voice-talk" type="button" disabled>Hold to talk</button>
  <button id="voice-sound" class="voice-btn" type="button" aria-pressed="true"></button>
  <p id="voice-status" class="voice-status" role="status" aria-live="polite">checking\u2026</p>
  <p id="voice-heard" class="voice-heard" aria-live="polite"></p>
`;
const slot = document.getElementById("dock-voice");
const lines = document.getElementById("dock-voice-lines");
if (slot) {
  // Part of the dock row: the buttons sit in the bar, the two lines of text
  // join the dock's own status line.
  root.className = "voice in-dock";
  slot.append(root);
} else {
  document.body.append(root);
}

const soundButton = root.querySelector("#voice-sound");
const talkButton = root.querySelector("#voice-talk");
talkButton.title = "Hold to talk, or hold V when no text field has focus";
const statusLine = root.querySelector("#voice-status");
const heardLine = root.querySelector("#voice-heard");
if (lines) lines.append(statusLine, heardLine);

// --------------------------------------------------------------------------- //
// Small helpers
// --------------------------------------------------------------------------- //
function readPref() {
  try {
    return localStorage.getItem(SOUND_KEY) !== "0";
  } catch {
    return true;
  }
}

function writePref(on) {
  try {
    localStorage.setItem(SOUND_KEY, on ? "1" : "0");
  } catch {
    /* private mode: the choice lasts for this page only */
  }
}

function setStatus(text, error = false) {
  statusLine.textContent = text;
  statusLine.classList.toggle("err", error);
}

function showSaid(who, text) {
  heardLine.textContent = `${who}: ${text}`;
  heardLine.title = text;
}

function shorten(text, limit = 70) {
  const one = String(text).replace(/\s+/g, " ").trim();
  return one.length > limit ? one.slice(0, limit - 1) + "\u2026" : one;
}

function renderSound() {
  soundButton.textContent = state.sound ? "Sound on" : "Sound off";
  soundButton.setAttribute("aria-pressed", state.sound ? "true" : "false");
  soundButton.title = state.sound ? "Narration is spoken" : "Narration is muted";
}

async function getJSON(path) {
  const response = await fetch(path, { cache: "no-store" });
  const payload = await response.json().catch(() => ({}));
  if (!response.ok) throw new Error(payload.error || response.statusText);
  return payload;
}

async function postJSON(path, body) {
  const response = await fetch(path, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(body),
  });
  const payload = await response.json().catch(() => ({}));
  if (!response.ok) throw new Error(payload.error || response.statusText);
  return payload;
}

// --------------------------------------------------------------------------- //
// Speech out: poll the lines, fetch one clip at a time, play in order
// --------------------------------------------------------------------------- //
function enqueue(line) {
  const text = String(line.text || "").trim();
  if (!text) return;
  showSaid("bot", text);
  if (!state.sound) return;
  state.queue.push({ text, at: Date.now() });
  pump();
}

async function pump() {
  if (state.busy || !state.sound) return;
  const next = state.queue.shift();
  if (!next) return;
  if (Date.now() - next.at > STALE_MS) {
    pump();
    return;
  }
  state.busy = true;
  try {
    const response = await fetch("/api/voice/tts", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ text: next.text }),
    });
    if (!response.ok) {
      const payload = await response.json().catch(() => ({}));
      setStatus(payload.error || "speech failed", true);
      return;
    }
    const url = URL.createObjectURL(await response.blob());
    await playClip(url);
  } catch (error) {
    setStatus(error.message, true);
  } finally {
    state.busy = false;
    pump();
  }
}

function playClip(url) {
  return new Promise((resolve) => {
    const done = () => {
      if (!finishClip) return;
      finishClip = null;
      player.onended = null;
      player.onerror = null;
      if (playerUrl) URL.revokeObjectURL(playerUrl);
      playerUrl = null;
      resolve();
    };
    finishClip = done;
    playerUrl = url;
    player.onended = done;
    player.onerror = done;
    player.src = url;
    Promise.resolve(player.play()).catch(() => {
      // Browsers refuse audio before the first user gesture on the page.
      state.blocked = true;
      setStatus("click the page once to allow sound");
      done();
    });
  });
}

async function poll() {
  if (!state.enabled || state.polling) return;
  state.polling = true;
  try {
    const query = state.initialized
      ? `?after=${encodeURIComponent(state.cursor ?? "")}`
      : "";
    const reply = await getJSON(`/api/voice/lines${query}`);
    state.initialized = true;
    for (const line of reply.lines || []) enqueue(line);
    if (reply.last) state.cursor = reply.last;
  } catch {
    /* the console link chip shows trouble; the next tick retries */
  } finally {
    state.polling = false;
  }
}

soundButton.addEventListener("click", () => {
  state.sound = !state.sound;
  writePref(state.sound);
  renderSound();
  if (state.sound) {
    setStatus("hold the mic or V to talk");
    pump();
  } else {
    state.queue.length = 0;
    player.pause();
    if (finishClip) finishClip();
    setStatus("narration muted");
  }
});

// --------------------------------------------------------------------------- //
// Speech in: hold the button, or hold V when no text field has focus
// --------------------------------------------------------------------------- //
function isTyping(target) {
  const node = target instanceof Element ? target : null;
  if (!node) return false;
  return (
    node.tagName === "INPUT" ||
    node.tagName === "TEXTAREA" ||
    node.tagName === "SELECT" ||
    node.isContentEditable
  );
}

function pickMime() {
  if (typeof MediaRecorder === "undefined") return "";
  return MIME_TYPES.find((type) => MediaRecorder.isTypeSupported(type)) || "";
}

async function startTalk() {
  if (!state.enabled || state.talking) return;
  if (!navigator.mediaDevices?.getUserMedia || typeof MediaRecorder === "undefined") {
    setStatus("recording is not supported in this browser", true);
    return;
  }
  if (state.permission === "denied") {
    setStatus("microphone permission denied", true);
    return;
  }
  state.talking = true;
  try {
    state.stream = await navigator.mediaDevices.getUserMedia({ audio: true });
  } catch {
    state.talking = false;
    state.permission = "denied";
    talkButton.disabled = true;
    setStatus("microphone permission denied", true);
    return;
  }
  if (!state.talking) {
    // Released while the permission prompt was open: never start a recorder
    // the operator can no longer stop.
    for (const track of state.stream.getTracks()) track.stop();
    state.stream = null;
    return;
  }
  const mime = pickMime();
  const recorder = new MediaRecorder(state.stream, mime ? { mimeType: mime } : undefined);
  const chunks = [];
  const startedAt = Date.now();
  recorder.ondataavailable = (event) => {
    if (event.data && event.data.size) chunks.push(event.data);
  };
  recorder.onstop = () => {
    const type = recorder.mimeType || chunks[0]?.type || "audio/webm";
    finishTalk(type, chunks, startedAt);
  };
  state.active = recorder;
  recorder.start();
  root.classList.add("recording");
  setStatus("listening\u2026");
}

function stopTalk() {
  if (!state.talking) return;
  state.talking = false;
  const recorder = state.active;
  state.active = null;
  root.classList.remove("recording");
  if (recorder) {
    try {
      recorder.stop();
    } catch {
      /* already stopped */
    }
  }
  if (state.stream) {
    for (const track of state.stream.getTracks()) track.stop();
    state.stream = null;
  }
}

async function finishTalk(type, chunks, startedAt) {
  const duration = Date.now() - startedAt;
  if (duration < MIN_RECORD_MS || chunks.length === 0) {
    setStatus("hold a little longer");
    return;
  }
  const blob = new Blob(chunks, { type: type.split(";")[0] || "audio/webm" });
  await sendAudio(blob);
}

async function sendAudio(blob) {
  setStatus("transcribing\u2026");
  let text = "";
  try {
    const response = await fetch("/api/voice/stt", {
      method: "POST",
      headers: { "Content-Type": blob.type },
      body: blob,
    });
    const payload = await response.json().catch(() => ({}));
    if (!response.ok) throw new Error(payload.error || response.statusText);
    text = String(payload.text || "").trim();
  } catch (error) {
    setStatus(error.message, true);
    return;
  }
  if (!text) {
    setStatus("heard nothing");
    return;
  }
  showSaid("you", text);
  // The dock may take the transcript into its task box and send it after a
  // short, cancellable pause; with no handler the request goes out directly.
  const handled = !document.dispatchEvent(new CustomEvent("praxion:voice-task", { detail: { text }, cancelable: true }));
  if (handled) {
    setStatus("heard, check the task box");
    return;
  }
  try {
    // The same request the task box uses: free text, no condition.
    await postJSON("/api/control", { action: "task", text });
    setStatus(`sent: ${shorten(text)}`);
  } catch (error) {
    setStatus(error.message, true);
  }
}

talkButton.addEventListener("pointerdown", (event) => {
  event.preventDefault();
  talkButton.setPointerCapture?.(event.pointerId);
  startTalk();
});
talkButton.addEventListener("pointerup", stopTalk);
talkButton.addEventListener("pointercancel", stopTalk);
talkButton.addEventListener("contextmenu", (event) => event.preventDefault());

document.addEventListener("keydown", (event) => {
  if (event.code !== "KeyV" || event.repeat || event.isComposing) return;
  if (event.metaKey || event.ctrlKey || event.altKey) return;
  if (isTyping(event.target)) return;
  event.preventDefault();
  startTalk();
});
document.addEventListener("keyup", (event) => {
  if (event.code === "KeyV") stopTalk();
});
window.addEventListener("blur", stopTalk);

// The first gesture anywhere unlocks playback for the rest of the page.
document.addEventListener(
  "click",
  () => {
    if (!state.blocked) return;
    state.blocked = false;
    setStatus("hold the mic or V to talk");
  },
  { capture: true },
);

// --------------------------------------------------------------------------- //
// Start
// --------------------------------------------------------------------------- //
async function boot() {
  renderSound();
  let reply = null;
  try {
    reply = await getJSON("/api/voice/status");
  } catch {
    setStatus("console offline", true);
    return;
  }
  state.enabled = !!reply.enabled;
  if (!state.enabled) {
    root.dataset.state = "off";
    root.title = "Set ELEVENLABS_API_KEY in .env to enable voice";
    soundButton.disabled = true;
    setStatus("voice off");
    return;
  }
  root.dataset.state = "on";
  root.title = `voice ${shorten(reply.voice_id || "", 24)}`;
  talkButton.disabled = false;
  setStatus("hold the mic or V to talk");
  poll();
  setInterval(poll, POLL_MS);
}

boot();
