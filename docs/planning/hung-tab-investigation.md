# Web client `RESULT_CODE_HUNG` after background idle — investigation

Status: **Phase 1 (root-cause investigation)** — gathering evidence before any fix.
Method: `superpowers:systematic-debugging` (no fixes without confirmed root cause).

## Symptom

Chrome desktop web client tabs are **found already dead** with
`Error code: RESULT_CODE_HUNG` after idling in the background. Longstanding
(not a recent regression). `RESULT_CODE_HUNG` = Chromium killed the renderer
because its **main thread stopped responding** to the browser's hang monitor —
i.e. a long **synchronous** task / microtask spin, or a GC death-spiral. Not OOM,
not network.

Key qualifier from the reporter: the tab dies **while backgrounded**, not on
return/foreground.

## Evidence gathered (static)

- **The XMPP runtime runs on the main thread** — no Web Worker (`new Worker`
  hits were `registerServiceWorker` false positives). Any main-thread block
  hangs the page.
- **Resume does unbounded, un-chunked, synchronous work** — `completeResumeBarrier`
  (`chat/src/lib/xmpp/client.ts:2628`) runs two synchronous loops over
  background-accumulated data: the MAM catch-up apply
  (`for (const page of pages.reverse()) applyRoomCatchupPage(...)`, 2999/2971)
  and the live-buffer drain (`for (const m of buffered) dispatchLiveBodyMessage(m)`,
  2649). The catch-up paging (2959/2988) is **uncapped** (pages back to `since`,
  which grows older with idle time); a sibling path caps at
  `DM_CALL_ACTIVITY_MAX_PAGES = 50` (line 234) — the main path does not.

## Hypotheses ruled OUT

- **Reconnect storm** — `scheduleReconnect` (835) uses real `setTimeout`
  exponential backoff, capped 60s, single-timer guard. Not a spin.
- **Wasm driver microtask spin** — driver `run` loop
  (`server/crates/waddle-xmpp-client-wasm/src/driver.rs:62`) is a `select!`
  handling one event per iteration, paced by macrotask WS delivery.

## Live hypotheses (given "dies while backgrounded")

The "found already dead in background" detail weakens the one-shot resume-burst
theory (which would hang on return) and favors mechanisms that run
**repeatedly/continuously in the background**:

1. **Connection flapping** — keepalive throttled in background → server idle
   timeout → reconnect → session-ready → catch-up → repeat. Continuous work.
2. **Memory leak / GC death-spiral** — unbounded growth over background time
   (per-reconnect handles/listeners, buffers) → GC saturates the main thread.
3. **Resume-burst (now secondary)** — a single large synchronous catch-up/drain
   if reconnect+session-ready happens while backgrounded.

## Next step: OpenTelemetry instrumentation (observe-only, no behavior change)

Add evidence-gathering instrumentation via the existing Faro/`@opentelemetry/api`
stack (`chat/src/lib/telemetry.ts` `report*` + `BrowserXmppClient` hooks wired in
`xmpp-instrumentation.ts`). Every signal tagged with `visibilityState` + ms-hidden
so we can see what happens **in the background**:

| Signal | Discriminates | Where |
|---|---|---|
| `longtask` PerformanceObserver (smoking gun) | synchronous-burst hang + when | telemetry health init |
| JS heap sampling (`usedJSHeapSize`, timer + on visibility change) | leak / GC death-spiral | telemetry health init |
| Reconnect-attempt counter (visibility, ms-hidden) | background flapping | `scheduleReconnect` hook |
| Catch-up span (rooms/dms/pages/messages/durationMs) | heavy/repeated catch-up | `runReconnectCatchup` hook |
| Resume-drain measurement (buffered size, durationMs) | synchronous drain burst | `completeResumeBarrier` hook |
| Visibility transitions (state, hiddenMs) | correlation backbone | telemetry health init |

Deploy → collect telemetry across several background-death occurrences → the
signal that fires (escalating longtasks `hidden` / reconnect flapping `hidden` /
unbounded heap growth `hidden`) names the root cause. Then fix in a follow-up.
