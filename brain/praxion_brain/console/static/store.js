// The page-wide store: the newest state, the evidence seen so far, and who listens.
//
// One place owns the stream's data so that a page built lazily (Mind, Run) can
// start from what the Live page has already received instead of refetching it.

const MAX_EVIDENCE = 3000;

export const store = {
  state: {},
  evidence: [],
  last: 0,
  link: false,
};

const stateListeners = new Set();
const evidenceListeners = new Set();
const linkListeners = new Set();

/** Subscribe to state events; returns the unsubscribe function. */
export function onState(listener) {
  stateListeners.add(listener);
  return () => stateListeners.delete(listener);
}

export function onEvidence(listener) {
  evidenceListeners.add(listener);
  return () => evidenceListeners.delete(listener);
}

export function onLink(listener) {
  linkListeners.add(listener);
  return () => linkListeners.delete(listener);
}

export function setState(state) {
  store.state = state || {};
  for (const listener of stateListeners) listener(store.state);
}

/** Append one evidence line (ignoring any already seen) and tell the listeners. */
export function addEvidence(entry) {
  if (entry.n <= store.last) return false;
  store.last = entry.n;
  store.evidence.push(entry);
  if (store.evidence.length > MAX_EVIDENCE) store.evidence.splice(0, store.evidence.length - MAX_EVIDENCE);
  for (const listener of evidenceListeners) listener(entry);
  return true;
}

/** Install the loaded history without notifying anyone: pages built afterwards read it. */
export function loadEvidence(lines) {
  for (const entry of lines) {
    if (entry.n <= store.last) continue;
    store.evidence.push(entry);
    store.last = entry.n;
  }
  if (store.evidence.length > MAX_EVIDENCE) store.evidence.splice(0, store.evidence.length - MAX_EVIDENCE);
}

export function setLink(on) {
  store.link = on;
  for (const listener of linkListeners) listener(on);
}
