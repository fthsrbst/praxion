#!/usr/bin/env node
// Summarize an `opencode run --format json` event log. Portable replacement for jq.
// Usage: node oc-events.js <text|error|tools|cost|summary> <events.jsonl>
const [mode, file] = process.argv.slice(2);
if (!mode || !file) {
  console.error("usage: oc-events.js <text|error|tools|cost|summary> <events.jsonl>");
  process.exit(2);
}

const events = [];
for (const line of require("fs").readFileSync(file, "utf8").split("\n")) {
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
