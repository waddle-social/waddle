import { describe, expect, test } from "bun:test";
import { MAX_FARO_MEASUREMENT_VALUE } from "../chat/src/lib/telemetry/measurement-contract";
import { sanitizeKnownMeasurementPayload } from "../chat/src/lib/telemetry/transport-schema";
import { parseBaselineCatalog } from "../scripts/switchable-baseline/catalog";
import { validateFaroSeries } from "../scripts/switchable-baseline/faro";

const catalog = parseBaselineCatalog(
  await Bun.file(new URL("../docs/observability/switchable-baseline-signals.json", import.meta.url)).json(),
);
const ackSignal = catalog.signals.find(({ id }) => id === "browser-message-ack-latency");
if (!ackSignal) throw new Error("missing acknowledgement latency signal");

describe("shared Faro measurement ceiling", () => {
  test("clamps browser values and rejects impossible aggregate percentiles at one bound", () => {
    const payload = sanitizeKnownMeasurementPayload({
      type: "chat.xmpp.message.acked.latency_ms",
      values: { latency_ms: MAX_FARO_MEASUREMENT_VALUE + 123_456 },
      context: {
        kind: "dm",
        deploymentEnvironment: "production",
        cluster: "waddle-cloud",
        namespace: "waddle",
        sourceId: "waddle-chat",
        release: "a".repeat(40),
      },
    });
    expect(payload?.values).toEqual({ latency_ms: MAX_FARO_MEASUREMENT_VALUE });

    const validRows = ["dm", "room"].map((kind) => ({
      attributes: { kind },
      count: 1,
      latencyMs: {
        p50: MAX_FARO_MEASUREMENT_VALUE,
        p95: MAX_FARO_MEASUREMENT_VALUE,
      },
    }));
    expect(() => validateFaroSeries(ackSignal, validRows)).not.toThrow();

    const impossibleRows = structuredClone(validRows);
    impossibleRows[0].latencyMs.p95 = MAX_FARO_MEASUREMENT_VALUE + 1;
    expect(() => validateFaroSeries(ackSignal, impossibleRows)).toThrow(
      `no greater than ${MAX_FARO_MEASUREMENT_VALUE}`,
    );
  });
});
