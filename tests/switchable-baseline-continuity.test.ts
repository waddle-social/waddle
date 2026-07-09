import { describe, expect, test } from "bun:test";
import {
  expectedTimestampGrid,
  normalizeQueryRangeResponse,
  parseBaselineCatalog,
  parseCollectorArguments,
  signalCollectionRequest,
  validateCollectionRequest,
} from "../scripts/switchable-baseline";

const catalog = parseBaselineCatalog(
  await Bun.file(new URL("../docs/observability/switchable-baseline-signals.json", import.meta.url)).json(),
);
const request = validateCollectionRequest(
  parseCollectorArguments([
    "--start", "2026-07-10T09:00:00Z",
    "--end", "2026-07-10T10:00:00Z",
    "--server-commit", "0123456789abcdef0123456789abcdef01234567",
    "--prometheus-job", "waddle-server",
    "--environment", "production",
    "--cluster", "waddle-cloud",
    "--namespace", "waddle",
    "--expected-replicas", "2",
  ]),
  catalog.minimumCollectionWindowMinutes,
  catalog.deploymentScope.maximumRangeLookbackSeconds,
);
const continuity = catalog.signals.find(({ id }) => id === "server-process-start-continuity");
if (!continuity) throw new Error("missing process continuity signal");

function response(values: Array<[number, string]>, extra: Record<string, unknown> = {}) {
  return {
    status: "success",
    data: {
      resultType: "matrix",
      result: [{ metric: {}, values }],
    },
    ...extra,
  };
}

describe("switchable baseline process continuity", () => {
  test("collects the typed pre-window and requires a constant process aggregate", () => {
    expect(continuity).toMatchObject({
      metricNames: ["waddle_process_start_time_seconds"],
      collectionLookbackSeconds: 3600,
      requiredStability: "constant",
    });
    const continuityRequest = signalCollectionRequest(continuity, request);
    expect(continuityRequest.startEpochSeconds).toBe(request.startEpochSeconds - 3600);
    const timestamps = expectedTimestampGrid(continuityRequest);
    expect(timestamps).toHaveLength(121);
    expect(() => normalizeQueryRangeResponse(
      continuity,
      response(timestamps.map((timestamp) => [timestamp, "3500000000.25"])),
      continuityRequest,
    )).not.toThrow();

    const changed = timestamps.map((timestamp, index): [number, string] => [
      timestamp,
      index === 60 ? "3500000001.25" : "3500000000.25",
    ]);
    expect(() => normalizeQueryRangeResponse(
      continuity,
      response(changed),
      continuityRequest,
    )).toThrow("must remain constant across the complete collection grid");
  });

  test("fails closed on qualified Prometheus results", () => {
    const continuityRequest = signalCollectionRequest(continuity, request);
    const values = expectedTimestampGrid(continuityRequest)
      .map((timestamp): [number, string] => [timestamp, "3500000000.25"]);
    expect(() => normalizeQueryRangeResponse(
      continuity,
      response(values, { warnings: ["partial response"] }),
      continuityRequest,
    )).toThrow("Prometheus returned warnings");
    expect(() => normalizeQueryRangeResponse(
      continuity,
      response(values, { infos: ["resolution reduced"] }),
      continuityRequest,
    )).toThrow("Prometheus returned infos");
    expect(() => normalizeQueryRangeResponse(
      continuity,
      response(values, { warnings: [], infos: [] }),
      continuityRequest,
    )).not.toThrow();
  });
});
