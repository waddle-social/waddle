import { describe, expect, test } from "bun:test";
import { mkdtemp, readFile, readdir, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import {
  collectBaselineArtifacts,
  expectedTimestampGrid,
  materializePrometheusSignals,
  normalizeQueryRangeResponse,
  parseCollectorArguments,
  readPrometheusConfiguration,
  requiredAttributeCombinations,
  sha256Hex,
  validateCollectionRequest,
  writeEvidencePairAtomically,
} from "../scripts/switchable-baseline";
import {
  catalog,
  parsedCatalog,
} from "./support/switchable-baseline-signals";

describe("switchable-alternative telemetry collection", () => {
  test("validates fixed collection provenance without exposing credentials", () => {
    const argumentsValue = parseCollectorArguments([
      "--start",
      "2026-07-10T09:00:00Z",
      "--end",
      "2026-07-10T10:00:00Z",
      "--server-commit",
      "0123456789abcdef0123456789abcdef01234567",
      "--prometheus-job",
      "waddle-server",
      "--environment",
      "production",
      "--cluster",
      "waddle-production",
      "--namespace",
      "waddle",
      "--expected-replicas",
      "2",
    ]);
    const request = validateCollectionRequest(
      argumentsValue,
      parsedCatalog.minimumCollectionWindowMinutes,
      parsedCatalog.deploymentScope.maximumRangeLookbackSeconds,
    );
    expect(request.start).toBe("2026-07-10T09:00:00.000Z");
    expect(request.end).toBe("2026-07-10T10:00:00.000Z");
    expect(request.durationMinutes).toBe(60);
    expect(request.job).toBe("waddle-server");
    expect(request.environment).toBe("production");
    expect(request.cluster).toBe("waddle-production");
    expect(request.namespace).toBe("waddle");
    expect(request.identityStartEpochSeconds).toBe(
      request.startEpochSeconds - 3600,
    );
    expect(request.expectedReplicas).toBe(2);
    expect(validateCollectionRequest(
      { ...argumentsValue, environment: "a".repeat(64), cluster: "b".repeat(64) },
      parsedCatalog.minimumCollectionWindowMinutes,
      parsedCatalog.deploymentScope.maximumRangeLookbackSeconds,
    ).environment).toHaveLength(64);
    expect(() =>
      validateCollectionRequest(
        { ...argumentsValue, environment: "a".repeat(65) },
        parsedCatalog.minimumCollectionWindowMinutes,
        parsedCatalog.deploymentScope.maximumRangeLookbackSeconds,
      ),
    ).toThrow("lowercase deployment label value");
    expect(() =>
      validateCollectionRequest(
        { ...argumentsValue, cluster: "b".repeat(65) },
        parsedCatalog.minimumCollectionWindowMinutes,
        parsedCatalog.deploymentScope.maximumRangeLookbackSeconds,
      ),
    ).toThrow("lowercase deployment label value");

    expect(() =>
      validateCollectionRequest(
        { ...argumentsValue, end: "2026-07-10T09:59:00Z" },
        parsedCatalog.minimumCollectionWindowMinutes,
        parsedCatalog.deploymentScope.maximumRangeLookbackSeconds,
      ),
    ).toThrow("at least 60 minutes");
    expect(() =>
      validateCollectionRequest(
        {
          ...argumentsValue,
          start: "2026-07-10T09:00:30Z",
          end: "2026-07-10T10:00:30Z",
        },
        parsedCatalog.minimumCollectionWindowMinutes,
        parsedCatalog.deploymentScope.maximumRangeLookbackSeconds,
      ),
    ).toThrow("align to the Prometheus query step");
    expect(() =>
      validateCollectionRequest(
        { ...argumentsValue, serverCommit: "short" },
        parsedCatalog.minimumCollectionWindowMinutes,
        parsedCatalog.deploymentScope.maximumRangeLookbackSeconds,
      ),
    ).toThrow("full 40-character lowercase Git commit SHA");
    expect(() =>
      validateCollectionRequest(
        { ...argumentsValue, environment: "Production" },
        parsedCatalog.minimumCollectionWindowMinutes,
        parsedCatalog.deploymentScope.maximumRangeLookbackSeconds,
      ),
    ).toThrow("lowercase deployment label value");
    expect(() =>
      validateCollectionRequest(
        { ...argumentsValue, expectedReplicas: "0" },
        parsedCatalog.minimumCollectionWindowMinutes,
        parsedCatalog.deploymentScope.maximumRangeLookbackSeconds,
      ),
    ).toThrow("positive integer");
    expect(() =>
      validateCollectionRequest(
        { ...argumentsValue, cluster: "unknown" },
        parsedCatalog.minimumCollectionWindowMinutes,
        parsedCatalog.deploymentScope.maximumRangeLookbackSeconds,
      ),
    ).toThrow("must not be unknown");
    expect(() =>
      validateCollectionRequest(
        { ...argumentsValue, namespace: "waddle-" },
        parsedCatalog.minimumCollectionWindowMinutes,
        parsedCatalog.deploymentScope.maximumRangeLookbackSeconds,
      ),
    ).toThrow("lowercase deployment label value");

    const materialized = materializePrometheusSignals(parsedCatalog, request);
    for (const signal of materialized) {
      expect(signal.query).not.toContain("{{");
      expect(signal.query).toContain(`job="${request.job}"`);
      expect(signal.query).toContain(request.environment);
      expect(signal.query).toContain(request.cluster);
      expect(signal.query).toContain(request.namespace);
    }
    const identity = materialized.find(
      ({ id }) => id === parsedCatalog.deploymentScope.targetSignalId,
    );
    expect(identity?.attributes).toEqual({
      commit: [request.serverCommit],
      exported_cluster: [request.cluster],
      exported_deployment_environment: [request.environment],
    });

    const configuration = readPrometheusConfiguration({
      GRAFANA_PROMETHEUS_URL: "https://prometheus.example/api/prom",
      GRAFANA_PROMETHEUS_USER: "tenant",
      GRAFANA_PROMETHEUS_API_KEY: "secret",
    });
    expect(configuration.baseUrl).toBe("https://prometheus.example/api/prom");
    expect(JSON.stringify(configuration)).not.toContain("Basic ");
    expect(() =>
      readPrometheusConfiguration({
        GRAFANA_PROMETHEUS_URL: "https://tenant:secret@prometheus.example",
        GRAFANA_PROMETHEUS_USER: "tenant",
        GRAFANA_PROMETHEUS_API_KEY: "secret",
      }),
    ).toThrow("must not contain credentials");
    expect(() =>
      readPrometheusConfiguration({
        GRAFANA_PROMETHEUS_URL: "http://prometheus.example",
        GRAFANA_PROMETHEUS_USER: "tenant",
        GRAFANA_PROMETHEUS_API_KEY: "secret",
      }),
    ).toThrow("must use HTTPS");
  });

  test("requires complete timestamp grids and exact closed series", () => {
    const signal = parsedCatalog.signals.find(
      ({ id }) => id === "live-delivery-channel-outcomes",
    );
    expect(signal).toBeDefined();
    const request = validateCollectionRequest(
      parseCollectorArguments([
        "--start", "2026-07-10T09:00:00Z",
        "--end", "2026-07-10T10:00:00Z",
        "--server-commit", "0123456789abcdef0123456789abcdef01234567",
        "--prometheus-job", "waddle-server",
        "--environment", "production",
        "--cluster", "waddle-production",
        "--namespace", "waddle",
        "--expected-replicas", "2",
      ]),
      parsedCatalog.minimumCollectionWindowMinutes,
      parsedCatalog.deploymentScope.maximumRangeLookbackSeconds,
    );
    const timestamps = expectedTimestampGrid(request);
    const combinations = requiredAttributeCombinations(signal!);
    const result = [...combinations].reverse().map((attributes, index) => ({
      metric: attributes,
      values: [...timestamps].reverse().map((timestamp) => [timestamp, String(index)]),
    }));

    const normalized = normalizeQueryRangeResponse(
      signal!,
      { status: "success", data: { resultType: "matrix", result } },
      request,
    );
    expect(normalized.series.map(({ attributes }) => attributes)).toEqual(
      combinations,
    );
    expect(normalized.series.every(({ canonicalEndSample }) =>
      canonicalEndSample.timestamp === request.endEpochSeconds
    )).toBeTrue();
    expect(normalized.series.every(({ samples }) => samples.length === 61)).toBeTrue();

    expect(() =>
      normalizeQueryRangeResponse(
        signal!,
        { status: "success", data: { resultType: "matrix", result: result.slice(1) } },
        request,
      ),
    ).toThrow("exact required series combinations");
    expect(() =>
      normalizeQueryRangeResponse(
        signal!,
        {
          status: "success",
          data: {
            resultType: "matrix",
            result: [{ metric: {}, values: timestamps.map((timestamp) => [timestamp, "1"]) }],
          },
        },
        request,
      ),
    ).toThrow("do not exactly match the catalog");
    expect(() =>
      normalizeQueryRangeResponse(
        signal!,
        {
          status: "success",
          data: {
            resultType: "matrix",
            result: result.map((series, index) =>
              index === 0 ? { ...series, values: series.values.slice(1) } : series
            ),
          },
        },
        request,
      ),
    ).toThrow("incomplete timestamp grid");
    expect(() =>
      normalizeQueryRangeResponse(
        signal!,
        {
          status: "success",
          data: {
            resultType: "matrix",
            result: result.map((series, index) =>
              index === 0
                ? { ...series, values: [[timestamps[0], "NaN"], ...series.values.slice(1)] }
                : series
            ),
          },
        },
        request,
      ),
    ).toThrow("non-finite sample");

    const freshnessSignal = parsedCatalog.signals.find(
      ({ id }) => id === "room-registry-sample-freshness",
    );
    expect(freshnessSignal).toBeDefined();
    const freshValues = timestamps.map((timestamp) => [timestamp, "59"]);
    expect(normalizeQueryRangeResponse(
      freshnessSignal!,
      {
        status: "success",
        data: {
          resultType: "matrix",
          result: [{ metric: {}, values: freshValues }],
        },
      },
      request,
    ).maximumAllowedValue).toBe(60);
    expect(() => normalizeQueryRangeResponse(
      freshnessSignal!,
      {
        status: "success",
        data: {
          resultType: "matrix",
          result: [{
            metric: {},
            values: timestamps.map((timestamp) => [timestamp, "61"]),
          }],
        },
      },
      request,
    )).toThrow("exceeds required maximum 60");
  });

  test("produces deterministic scoped evidence bytes from query fixtures", async () => {
    const fixtureCatalog = {
      schemaVersion: 1,
      milestone: "switchable-alternative",
      minimumCollectionWindowMinutes: 60,
      privacy: {
        forbiddenAttributeFragments: ["user", "token"],
        maximumValuesPerAttribute: 4,
        prohibitedPayloads: ["message content"],
      },
      deploymentScope: {
        identityMetric: "waddle_build_info",
        targetSignalId: "server-deployment-identity-targets",
        maximumRangeLookbackSeconds: 3600,
        requiredLabels: ["job", "deployment_environment", "cluster", "namespace"],
        queryPlaceholders: ["{{scope}}"],
      },
      signals: [
        {
          id: "server-deployment-identity-targets",
          owner: "operations",
          source: "prometheus",
          collection: "automated",
          kind: "gauge",
          metricNames: ["waddle_build_info"],
          attributes: {
            commit: ["{{commit}}"],
            exported_cluster: ["{{cluster}}"],
            exported_deployment_environment: ["{{environment}}"],
          },
          unit: "target",
          query: "count by (commit, exported_cluster, exported_deployment_environment) (count_over_time(waddle_build_info{{scope}}[60s]) > 0)",
          window: "point",
          collectionLookbackSeconds: 3600,
          interpretation: "Matching deployment targets.",
          limitations: "Identity only.",
        },
        {
          id: "fixture-outcomes",
          owner: "operations",
          source: "prometheus",
          collection: "automated",
          kind: "counter",
          metricNames: ["fixture_operations_total"],
          attributes: { outcome: ["error", "success"] },
          unit: "operation",
          query: "sum by (outcome) (increase(fixture_operations_total{{scope}}[1h]))",
          window: "1h",
          interpretation: "Fixture operations.",
          limitations: "Test fixture only.",
        },
        {
          id: "browser-fixture",
          owner: "web-client",
          source: "faro",
          collection: "manual-export",
          kind: "event",
          metricNames: ["chat.fixture"],
          attributes: {},
          unit: "event",
          query: "Faro event chat.fixture",
          window: "1h",
          faroQuery: {
            sourceId: "waddle-chat",
            signalNames: ["chat.fixture"],
            groupBy: [],
            aggregates: ["count"],
          },
          interpretation: "Fixture browser event.",
          limitations: "Test fixture only.",
        },
      ],
    };
    const rawCatalog = `${JSON.stringify(fixtureCatalog, null, 2)}\n`;
    const argumentsList = [
      "--start", "2026-07-10T09:00:00Z",
      "--end", "2026-07-10T10:00:00Z",
      "--server-commit", "0123456789abcdef0123456789abcdef01234567",
      "--prometheus-job", "waddle-server",
      "--environment", "production",
      "--cluster", "waddle-production",
      "--namespace", "waddle",
      "--expected-replicas", "2",
    ];
    const environment = {
      GRAFANA_PROMETHEUS_URL: "https://prometheus.example/api/prom",
      GRAFANA_PROMETHEUS_USER: "tenant",
      GRAFANA_PROMETHEUS_API_KEY: "secret",
    };
    const makeFetcher = (
      reverse: boolean,
      targetCount = "2",
      targetCommit: string | string[] = "0123456789abcdef0123456789abcdef01234567",
      targetEnvironment = "production",
      shortLivedUnexpectedRevision = false,
    ) => async (
      input: string | URL | Request,
      init?: RequestInit,
    ) => {
      const url = input instanceof URL
        ? input
        : new URL(typeof input === "string" ? input : input.url);
      expect(new Headers(init?.headers).get("Authorization")).toBe(
        "Basic dGVuYW50OnNlY3JldA==",
      );
      expect(url.searchParams.get("query")).not.toContain("{{");
      const start = Number(url.searchParams.get("start"));
      const end = Number(url.searchParams.get("end"));
      const step = Number(url.searchParams.get("step"));
      const timestamps: number[] = [];
      for (let timestamp = start; timestamp <= end; timestamp += step) {
        timestamps.push(timestamp);
      }
      const values = timestamps.map((timestamp) => [timestamp, targetCount]);
      const query = url.searchParams.get("query") ?? "";
      const evidenceStart = Date.parse("2026-07-10T09:00:00Z") / 1_000;
      expect(start).toBe(
        query.startsWith("count by (commit,")
          ? evidenceStart - fixtureCatalog.deploymentScope.maximumRangeLookbackSeconds
          : evidenceStart,
      );
      const result = query.startsWith("count by (commit,")
        ? (Array.isArray(targetCommit) ? targetCommit : [targetCommit]).map(
          (commit, index) => ({
            metric: {
                commit,
                exported_cluster: "waddle-production",
                exported_deployment_environment: targetEnvironment,
            },
            values: shortLivedUnexpectedRevision && index > 0
              ? [[values[Math.floor(values.length / 2)][0], "1"]]
              : reverse ? [...values].reverse() : values,
            }),
          )
        : ["error", "success"].map((outcome, index) => ({
            metric: { outcome },
            values: (reverse ? [...timestamps].reverse() : timestamps).map(
              (timestamp) => [timestamp, String(index)],
            ),
          }));
      if (reverse && !query.startsWith("count by (commit,")) result.reverse();
      return new Response(JSON.stringify({
        status: "success",
        data: { resultType: "matrix", result },
      }));
    };

    const first = await collectBaselineArtifacts({
      rawCatalog,
      argumentsList,
      environment,
      fetcher: makeFetcher(false),
      catalogAtCommit: async () => rawCatalog,
    });
    const second = await collectBaselineArtifacts({
      rawCatalog,
      argumentsList,
      environment,
      fetcher: makeFetcher(true),
      catalogAtCommit: async () => rawCatalog,
    });

    expect(second.jsonEvidence).toBe(first.jsonEvidence);
    expect(second.markdownEvidence).toBe(first.markdownEvidence);
    expect(first.jsonSha256).toBe(sha256Hex(first.jsonEvidence));
    expect(first.jsonSha256).toBe("d09a152fd51dcb314feebc05cde9b69d5b270a4bcdeb2f637adb5a8519682920");
    expect(sha256Hex(first.markdownEvidence)).toBe("4502d7a6f369f4872f09f92286d580c3870e4348494bbb31375f35290ce73d01");
    expect(first.evidence.deploymentScope).toEqual({
      job: "waddle-server",
      environment: "production",
      cluster: "waddle-production",
      namespace: "waddle",
      expectedReplicas: 2,
      identityMetric: "waddle_build_info",
      targetSignalId: "server-deployment-identity-targets",
      identityLookbackSeconds: 3600,
    });
    expect(first.jsonEvidence).not.toContain("{{");
    await expect(collectBaselineArtifacts({
      rawCatalog,
      argumentsList,
      environment,
      fetcher: makeFetcher(false, "1"),
      catalogAtCommit: async () => rawCatalog,
    })).rejects.toThrow("does not match expected replicas 2");
    await expect(collectBaselineArtifacts({
      rawCatalog,
      argumentsList,
      environment,
      fetcher: makeFetcher(false, "3"),
      catalogAtCommit: async () => rawCatalog,
    })).rejects.toThrow("does not match expected replicas 2");

    await expect(collectBaselineArtifacts({
      rawCatalog,
      argumentsList,
      environment,
      fetcher: makeFetcher(false, "2", "fedcba9876543210fedcba9876543210fedcba98"),
      catalogAtCommit: async () => rawCatalog,
    })).rejects.toThrow("undeclared attribute value");
    await expect(collectBaselineArtifacts({
      rawCatalog,
      argumentsList,
      environment,
      fetcher: makeFetcher(
        false,
        "2",
        [
          "0123456789abcdef0123456789abcdef01234567",
          "fedcba9876543210fedcba9876543210fedcba98",
        ],
        "production",
        true,
      ),
      catalogAtCommit: async () => rawCatalog,
    })).rejects.toThrow("undeclared attribute value");
    await expect(collectBaselineArtifacts({
      rawCatalog,
      argumentsList,
      environment,
      fetcher: makeFetcher(
        false,
        "2",
        "0123456789abcdef0123456789abcdef01234567",
        "unknown",
      ),
      catalogAtCommit: async () => rawCatalog,
    })).rejects.toThrow("undeclared attribute value");
    await expect(collectBaselineArtifacts({
      rawCatalog,
      argumentsList,
      environment,
      fetcher: makeFetcher(false, "2", [
        "0123456789abcdef0123456789abcdef01234567",
        "fedcba9876543210fedcba9876543210fedcba98",
      ]),
      catalogAtCommit: async () => rawCatalog,
    })).rejects.toThrow("undeclared attribute value");

    await expect(collectBaselineArtifacts({
      rawCatalog,
      argumentsList,
      environment,
      fetcher: makeFetcher(false),
      catalogAtCommit: async () => `${rawCatalog} `,
    })).rejects.toThrow("do not match the catalog at the asserted commit");

    const outputDirectory = await mkdtemp(join(tmpdir(), "waddle-baseline-"));
    try {
      await writeEvidencePairAtomically(outputDirectory, "old-json", "old-markdown");
      await expect(writeEvidencePairAtomically(
        outputDirectory,
        first.jsonEvidence,
        first.markdownEvidence,
      )).rejects.toThrow("refuses to replace existing output");
      expect(await readFile(resolve(outputDirectory, "telemetry-baseline.json"), "utf8"))
        .toBe("old-json");
      expect(await readFile(resolve(outputDirectory, "telemetry-baseline.md"), "utf8"))
        .toBe("old-markdown");
      expect((await readdir(outputDirectory)).sort()).toEqual([
        "telemetry-baseline.json",
        "telemetry-baseline.md",
      ]);
    } finally {
      await rm(outputDirectory, { recursive: true, force: true });
    }
  });
});
