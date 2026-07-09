import { readdir } from "node:fs/promises";
import { resolve } from "node:path";
import { parseBaselineCatalog } from "../../scripts/switchable-baseline";

export const repositoryRoot = resolve(import.meta.dir, "../..");
const catalogPath = resolve(
  repositoryRoot,
  "docs/observability/switchable-baseline-signals.json",
);
export const catalog = await Bun.file(catalogPath).json();
export const parsedCatalog = parseBaselineCatalog(catalog);

async function rustSourceFiles(directory: string): Promise<string[]> {
  const entries = await readdir(directory, { withFileTypes: true });
  return (
    await Promise.all(entries.map(async (entry) => {
      const path = resolve(directory, entry.name);
      if (entry.isDirectory()) return rustSourceFiles(path);
      return entry.isFile() && entry.name.endsWith(".rs") ? [path] : [];
    }))
  ).flat();
}

const prometheusModuleRoot = resolve(
  repositoryRoot,
  "server/crates/waddle-xmpp/src/prometheus",
);
export const prometheusSource = (
  await Promise.all(
    [
      resolve(repositoryRoot, "server/crates/waddle-xmpp/src/prometheus.rs"),
      ...(await rustSourceFiles(prometheusModuleRoot)),
      resolve(repositoryRoot, "server/crates/waddle-server/src/server/health.rs"),
    ].map((path) => Bun.file(path).text()),
  )
).join("\n");
const telemetryModuleRoot = resolve(repositoryRoot, "chat/src/lib/telemetry");
export const faroSource = (
  await Promise.all([
    resolve(repositoryRoot, "chat/src/lib/telemetry.ts"),
    ...(await readdir(telemetryModuleRoot, { withFileTypes: true }))
      .filter((entry) => entry.isFile() && entry.name.endsWith(".ts"))
      .map((entry) => resolve(telemetryModuleRoot, entry.name)),
  ].map((path) => Bun.file(path).text()))
).join("\n");

export type Signal = {
  id: string;
  owner: string;
  source: "prometheus" | "faro";
  collection: "automated" | "manual-export";
  kind: string;
  metricNames: string[];
  attributes: Record<string, string[]>;
  unit: string;
  query: string;
  window: string;
  collectionLookbackSeconds?: number;
  requiredStability?: "constant";
  minimumAllowedValue?: number;
  maximumAllowedValue?: number;
  interpretation: string;
  limitations: string;
};

export const normalize = (value: string) =>
  value.replace(/[-_.]/g, "").toLowerCase();
