// Fetch helpers and the live stream.

export async function getJSON(path) {
  const response = await fetch(path, { cache: "no-store" });
  if (!response.ok) throw new Error((await response.json().catch(() => ({}))).error || response.statusText);
  return response.json();
}

export async function control(body) {
  const response = await fetch("/api/control", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(body),
  });
  const payload = await response.json().catch(() => ({}));
  if (!response.ok) throw new Error(payload.error || response.statusText);
  return payload;
}

/** Open the SSE stream from line `since`; EventSource reconnects by itself and
 * sends `Last-Event-ID`, which is where the server resumes. */
export function stream(since, handlers) {
  const source = new EventSource(`/api/stream?since=${since}`);
  source.addEventListener("evidence", (e) => handlers.evidence(JSON.parse(e.data)));
  source.addEventListener("state", (e) => handlers.state(JSON.parse(e.data)));
  source.onopen = () => handlers.link(true);
  source.onerror = () => handlers.link(false);
  return source;
}
