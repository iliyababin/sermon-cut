import { useEffect, useRef } from "react";

/** Something to finish before the user leaves a screen or the app (e.g.
 * saving review edits). Throws, with a message for the user, if it can't. */
export type BeforeLeave = () => Promise<void>;

const handlers = new Set<BeforeLeave>();

/** Add a handler; returns the function that removes it. */
export function registerBeforeLeave(handler: BeforeLeave): () => void {
  handlers.add(handler);
  return () => {
    handlers.delete(handler);
  };
}

/** Run every handler. Rejects with the first failure, after all have run. */
export async function runBeforeLeave(): Promise<void> {
  const results = await Promise.allSettled([...handlers].map((h) => h()));
  const failed = results.find((r): r is PromiseRejectedResult => r.status === "rejected");
  if (failed) throw failed.reason;
}

/** Register `handler` while the component is mounted (always its latest
 * version). */
export function useBeforeLeave(handler: BeforeLeave) {
  const latest = useRef(handler);
  latest.current = handler;
  useEffect(() => registerBeforeLeave(() => latest.current()), []);
}
