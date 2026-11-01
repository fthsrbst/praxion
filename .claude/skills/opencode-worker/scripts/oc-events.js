#!/usr/bin/env node
// Summarize an `opencode run --format json` event log. Portable replacement for jq.
// Usage: node oc-events.js <text|error|tools|cost|summary> <events.jsonl>
//        node oc-events.js session-id <title> [session-list.json]
//          Reads `opencode session list --format json` (stdin, or the given file) and
//          prints the id of the newest session titled <title>. Prints nothing if none.
const fs = require("fs");
const [mode, arg] = process.argv.slice(2);

if (mode === "session-id") {
  if (arg === undefined) {
    console.error("usage: oc-events.js session-id <title> [session-list.json]");
    process.exit(2);
  }
  // fd 0 is stdin; the optional file argument keeps this testable without a pipe.
  const listFile = process.argv[4];
  let sessions;
  try {
    sessions = JSON.parse(fs.readFileSync(listFile === undefined ? 0 : listFile, "utf8"));
  } catch {
    console.error("session-id: invalid session list JSON");
    process.exit(2);
  }
  // Sessions are listed newest first, but sort explicitly: several runs of the same
  // brief share a title, and the one we just started is the newest.
  const newest = sessions
    .filter(s => s.title === arg)
    .sort((a, b) => (b.updated ?? b.created ?? 0) - (a.updated ?? a.created ?? 0))[0];
  if (newest) console.log(newest.id);
  process.exit(0);
}

if (!mode || !arg) {
  console.error("usage: oc-events.js <text|error|tools|cost|summary> <events.jsonl>");
  process.exit(2);
}

const events = [];
for (const line of fs.readFileSync(arg, "utf8").split("\n")) {
  try { events.push(JSON.parse(line)); } catch { /* non-JSON lines (stderr) */ }
}

const text = () => events.filter(e => e.type === "text").map(e => e.part.text);
const errors = () => events.filter(e => e.type === "error").map(e => e.error.message ?? JSON.stringify(e.error));
const tools = () => events.filter(e => e.type === "tool_use").map(e => `${e.part.tool} ${e.part.state.status}`);
const cost = () => events.filter(e => e.type === "step_finish").reduce((sum, e) => sum + (e.part.cost ?? 0), 0);

const out = {
  text: () => text().join("\n"),
  error: () => errors().join("\n"),
  tools: () => tools().join("\n"),
  cost: () => String(cost()),
  summary: () => [...tools().map(t => `tool: ${t}`), ...errors().map(m => `error: ${m}`), `cost: ${cost()}`].join("\n"),
}[mode];
if (!out) { console.error(`unknown mode: ${mode}`); process.exit(2); }
const result = out();
if (result) console.log(result);
