import type { Faro } from "@grafana/faro-web-sdk";
import type { FaroBuildIdentityScope } from "../../build-identity-contract";

export type FaroDeploymentScope = FaroBuildIdentityScope;

let faro: Faro | null = null;
let configuredTrustedSpanOrigins = new Set<string>();
let gateZeroFaroScope: FaroDeploymentScope | null = null;

export function getFaro(): Faro | null { return faro; }
export function setFaro(instance: Faro | null): void { faro = instance; }
export function getConfiguredTrustedSpanOrigins(): Set<string> {
  return new Set(configuredTrustedSpanOrigins);
}
export function setConfiguredTrustedSpanOrigins(origins: ReadonlySet<string>): void {
  configuredTrustedSpanOrigins = new Set(origins);
}
export function getGateZeroFaroScope(): FaroDeploymentScope | null {
  return gateZeroFaroScope ? { ...gateZeroFaroScope } : null;
}
export function setGateZeroFaroScope(scope: FaroDeploymentScope | null): void {
  gateZeroFaroScope = scope ? { ...scope } : null;
}
export function observeTelemetry(operation: () => void): void {
  try { operation(); } catch {
    // A broken observability pipeline must remain invisible to product flow.
  }
}
export function pushEventObserveOnly(name: string, attributes?: Record<string, string>): void {
  const instance = faro;
  if (!instance) return;
  observeTelemetry(() => instance.api.pushEvent(name, attributes));
}
export function pushMeasurementObserveOnly(
  measurement: { type: string; values: Record<string, number> },
  options?: { context?: Record<string, string> },
): void {
  const instance = faro;
  if (!instance) return;
  observeTelemetry(() => instance.api.pushMeasurement(measurement, options));
}
