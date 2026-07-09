import { describe, expect, test } from "bun:test";
import { existsSync } from "node:fs";
import { resolve } from "node:path";
import {
  assertGateZeroSignalSet,
  parseBaselineCatalog,
} from "../scripts/switchable-baseline";
import {
  catalog,
  faroSource,
  normalize,
  parsedCatalog,
  prometheusSource,
  repositoryRoot,
  type Signal,
} from "./support/switchable-baseline-signals";

describe("switchable-alternative telemetry catalog", () => {
  test("defines a unique complete implementation-backed signal catalog", () => {
    expect(() => assertGateZeroSignalSet(parsedCatalog)).not.toThrow();
    expect(catalog.schemaVersion).toBe(1);
    expect(catalog.milestone).toBe("switchable-alternative");
    expect(catalog.minimumCollectionWindowMinutes).toBeGreaterThanOrEqual(60);
    expect(catalog.signals.length).toBeGreaterThanOrEqual(10);
    expect(catalog.deploymentScope).toEqual({
      identityMetric: "waddle_build_info",
      targetSignalId: "server-deployment-identity-targets",
      maximumRangeLookbackSeconds: 3600,
      requiredLabels: ["job", "deployment_environment", "cluster", "namespace"],
      queryPlaceholders: ["{{scope}}"],
    });

    const ids = new Set<string>();
    for (const signal of catalog.signals as Signal[]) {
      expect(signal.id).toMatch(/^[a-z][a-z0-9-]+$/);
      expect(ids.has(signal.id)).toBeFalse();
      ids.add(signal.id);
      expect(signal.owner).toMatch(/^[a-z][a-z0-9-]+$/);
      expect(["prometheus", "faro"]).toContain(signal.source);
      expect(["automated", "manual-export"]).toContain(signal.collection);
      expect(signal.metricNames.length).toBeGreaterThan(0);
      expect(new Set(signal.metricNames).size).toBe(signal.metricNames.length);
      for (const field of ["kind", "unit", "query", "window", "interpretation", "limitations"] as const) {
        expect(signal[field].trim().length).toBeGreaterThan(0);
      }

      for (const metricName of signal.metricNames) {
        expect(signal.query).toContain(metricName);
        if (signal.source === "prometheus") {
          expect(metricName).toMatch(/^[a-z][a-z0-9_:]*$/);
          expect(prometheusSource).toContain(metricName);
        } else {
          expect(metricName).toMatch(/^chat\.[a-z0-9_.]+$/);
          expect(faroSource).toContain(`"${metricName}"`);
        }
      }
      if (signal.source === "prometheus") {
        for (const metricName of signal.metricNames) {
          expect(signal.query).toContain(`${metricName}{{scope}}`);
        }
        expect(signal.query).not.toContain("and on(");
      }
    }

    const freshness = catalog.signals.find(
      (signal: Signal) => signal.id === "room-registry-sample-freshness",
    );
    expect(freshness).toMatchObject({
      metricNames: [
        "waddle_room_registry_sample_last_success_unixtime_seconds",
      ],
      minimumAllowedValue: 0,
      maximumAllowedValue: 60,
    });
    const identity = catalog.signals.find(
      (signal: Signal) => signal.id === "server-deployment-identity-targets",
    );
    expect(identity?.query).toContain(
      "count_over_time(waddle_build_info{{scope}}[60s]) > 0",
    );
  });

  test("allows only declared low-cardinality dimensions", () => {
    const forbidden = catalog.privacy.forbiddenAttributeFragments.map(normalize);
    const maximumValues = catalog.privacy.maximumValuesPerAttribute;
    expect(maximumValues).toBeGreaterThan(0);
    expect(maximumValues).toBeLessThanOrEqual(32);

    for (const signal of catalog.signals as Signal[]) {
      for (const [attribute, values] of Object.entries(signal.attributes)) {
        const normalizedAttribute = normalize(attribute);
        for (const fragment of forbidden) {
          expect(normalizedAttribute.includes(fragment)).toBeFalse();
        }
        expect(values.length).toBeGreaterThan(0);
        expect(values.length).toBeLessThanOrEqual(maximumValues);
        expect(new Set(values).size).toBe(values.length);
        for (const value of values) {
          if (["{{commit}}", "{{environment}}", "{{cluster}}"].includes(value)) {
            continue;
          }
          expect(value).toMatch(/^[a-z][a-z0-9_-]*$/);
        }
      }

      if (signal.source === "prometheus") {
        for (const selector of signal.query.matchAll(/\{([^}]*)\}/g)) {
          for (const label of selector[1].matchAll(/([a-zA-Z_:][a-zA-Z0-9_:]*)\s*(?:=|!=|=~|!~)/g)) {
            const normalizedLabel = normalize(label[1]);
            for (const fragment of forbidden) {
              expect(normalizedLabel.includes(fragment)).toBeFalse();
            }
          }
        }
      }
    }
  });

  test("pins the privacy and replica-identity implementation surfaces", async () => {
    const traceSource = await Bun.file(
      resolve(repositoryRoot, "server/crates/waddle-server/src/server/trace.rs"),
    ).text();
    const serverTelemetry = await Bun.file(
      resolve(repositoryRoot, "server/crates/waddle-server/src/telemetry.rs"),
    ).text();
    const authSessionSource = await Bun.file(
      resolve(repositoryRoot, "server/crates/waddle-server/src/auth/session.rs"),
    ).text();
    const saslSource = await Bun.file(
      resolve(
        repositoryRoot,
        "server/crates/waddle-server/src/server/routes/websocket/sasl.rs",
      ),
    ).text();
    const directArchiveSource = await Bun.file(
      resolve(
        repositoryRoot,
        "server/crates/waddle-server/src/server/routes/interpret/direct_archive.rs",
      ),
    ).text();
    const roomArchiveSource = await Bun.file(
      resolve(
        repositoryRoot,
        "server/crates/waddle-server/src/server/routes/interpret/groupchat_archive.rs",
      ),
    ).text();
    const xmppAuthSource = await Bun.file(
      resolve(repositoryRoot, "server/crates/waddle-server/src/server/xmpp_auth_state.rs"),
    ).text();
    const chartDeployment = await Bun.file(
      resolve(repositoryRoot, "server/charts/waddle-server/templates/deployment.yaml"),
    ).text();
    const alloyRelease = await Bun.file(
      resolve(
        repositoryRoot,
        "infrastructure/waddle.cloud/gitops/grafana-alloy/helmrelease.yaml",
      ),
    ).text();

    expect(traceSource).toContain("return path.to_string()");
    expect(traceSource).not.toContain("return uri.to_string()");
    expect(authSessionSource).not.toContain("#[instrument(skip(self))]");
    expect(authSessionSource).not.toContain("session_id = %");
    expect(xmppAuthSource).not.toContain("token_prefix");
    expect(saslSource).toContain("increment_auth_terminal_attempt");
    expect(directArchiveSource).toContain("increment_message_archive_attempt");
    expect(roomArchiveSource).toContain("increment_message_archive_attempt");
    for (const key of [
      "service.instance.id",
      "service.version",
      "k8s.pod.name",
      "k8s.namespace.name",
      "k8s.cluster.name",
      "deployment.environment.name",
    ]) {
      expect(serverTelemetry).toContain(`"${key}"`);
    }
    for (const variable of [
      "OTEL_SERVICE_INSTANCE_ID",
      "K8S_POD_UID",
      "K8S_POD_NAME",
      "K8S_NAMESPACE_NAME",
      "DEPLOYMENT_ENVIRONMENT_NAME",
      "DEPLOYMENT_CLUSTER_NAME",
    ]) {
      expect(chartDeployment).toContain(`name: ${variable}`);
    }
    expect(alloyRelease).toContain(
      'source_labels = ["__meta_kubernetes_pod_uid"]',
    );
    expect(alloyRelease).toContain("honor_labels    = false");
    for (const label of [
      "instance",
      "job",
      "namespace",
      "deployment_environment",
      "cluster",
    ]) {
      expect(alloyRelease).toMatch(
        new RegExp(`target_label\\s+=\\s+"${label}"`),
      );
    }
    expect(existsSync(resolve(repositoryRoot, "docs/runbooks/switchable-baseline.md"))).toBeTrue();
  });

  test("enforces runtime catalog privacy and deployment identity contracts", () => {
    const unsafeAttribute = structuredClone(catalog);
    unsafeAttribute.signals.find((signal: Signal) => signal.source === "prometheus")
      .attributes.user_id = ["leak"];
    expect(() => parseBaselineCatalog(unsafeAttribute)).toThrow(
      "contains forbidden privacy fragment",
    );

    const excessiveValues = structuredClone(catalog);
    excessiveValues.signals.find((signal: Signal) =>
      Object.keys(signal.attributes).length > 0
    ).attributes.outcome = Array.from(
      { length: catalog.privacy.maximumValuesPerAttribute + 1 },
      (_, index) => `value_${index}`,
    );
    expect(() => parseBaselineCatalog(excessiveValues)).toThrow(
      "exceeds maximumValuesPerAttribute",
    );

    const emptyPayloadDescription = structuredClone(catalog);
    emptyPayloadDescription.privacy.prohibitedPayloads = ["   "];
    expect(() => parseBaselineCatalog(emptyPayloadDescription)).toThrow(
      "prohibited payload descriptions must be non-empty",
    );

    const unsafeSelector = structuredClone(catalog);
    const selectorSignal = unsafeSelector.signals.find(
      (signal: Signal) => signal.source === "prometheus",
    );
    selectorSignal.query = selectorSignal.query.replace("{", '{user_id="leak",');
    expect(() => parseBaselineCatalog(unsafeSelector)).toThrow(
      "query selector label user_id contains forbidden privacy fragment",
    );

    const unscopedMetric = structuredClone(catalog);
    const unscopedSignal = unscopedMetric.signals.find(
      (signal: Signal) => signal.source === "prometheus",
    );
    unscopedSignal.query = unscopedSignal.query.replace("{{scope}}", "");
    expect(() => parseBaselineCatalog(unscopedMetric)).toThrow(
      "must apply {{scope}} directly",
    );

    const adHocSelector = structuredClone(catalog);
    const adHocSignal = adHocSelector.signals.find(
      (signal: Signal) => signal.id === "connection-registry-entries",
    );
    adHocSignal.query = adHocSignal.query.replace(
      "{{scope}}",
      '{{scope}} or waddle_connected_users{job="other"}',
    );
    expect(() => parseBaselineCatalog(adHocSelector)).toThrow(
      "must not define ad-hoc label selectors",
    );

    const extraTargetMetric = structuredClone(catalog);
    const extraTargetSignal = extraTargetMetric.signals.find(
      (signal: Signal) => signal.id === extraTargetMetric.deploymentScope.targetSignalId,
    );
    extraTargetSignal.metricNames = ["unrelated_metric"];
    extraTargetSignal.query = extraTargetSignal.query.replace(
      "waddle_build_info",
      "unrelated_metric",
    );
    expect(() => parseBaselineCatalog(extraTargetMetric)).toThrow(
      "metricNames must equal exactly identityMetric",
    );

    const shortIdentityLookback = structuredClone(catalog);
    shortIdentityLookback.deploymentScope.maximumRangeLookbackSeconds = 1800;
    expect(() => parseBaselineCatalog(shortIdentityLookback)).toThrow(
      "does not cover the maximum Prometheus range lookback",
    );
  });
});
