/**
 * Glue between `BrowserXmppClient`'s telemetry hook API and Grafana Faro.
 *
 * Every function this installs is a no-op when Faro isn't initialized —
 * the SDK wrapper in `@/lib/telemetry` handles that at the report-site,
 * so tests and non-prod builds can `installInstrumentation()` freely
 * without any beacon traffic.
 */
import type { BrowserXmppClient } from "@/lib/xmpp-client";
import {
  reportMessageAcked,
  reportMessageFailed,
  reportQueueDepthChange,
  reportSendEnqueued,
  reportSessionLifecycle,
  reportStatusChange,
} from "@/lib/telemetry";

export function installInstrumentation(client: BrowserXmppClient): void {
  client.onMessageAcked((id, meta) => {
    reportMessageAcked({ id, kind: meta.kind, latencyMs: meta.latencyMs });
  });
  client.onMessageDeliveryFailed((id, meta) => {
    reportMessageFailed({ id, kind: meta.kind });
  });
  client.onSessionLifecycle((event) => {
    reportSessionLifecycle({ type: event.type });
  });
  client.onStatus((status, meta) => {
    reportStatusChange({
      state: status.state,
      detail: status.detail,
      reconnectDurationMs: meta.reconnectDurationMs,
    });
  });
  client.onSendEnqueued((info) => {
    reportSendEnqueued({ kind: info.kind, reason: info.reason });
  });
  client.onQueueDepthChange((depth) => {
    reportQueueDepthChange(depth);
  });
}
