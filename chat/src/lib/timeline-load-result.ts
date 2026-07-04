/**
 * Outcome of a wholesale timeline `loadMessages` call.
 *
 * - "loaded": the timeline was rebuilt from a fetched MAM page.
 * - "aborted": a newer request or a conversation switch superseded this
 *   call — the timeline now belongs to that newer owner; do not react.
 * - "failed": the fetch errored and the catch reset the timeline to
 *   queued-only. Callers that reloaded an existing timeline (#1180
 *   catch-up fallback) must restore their pre-reload snapshot.
 */
export type TimelineLoadResult = "loaded" | "aborted" | "failed";
