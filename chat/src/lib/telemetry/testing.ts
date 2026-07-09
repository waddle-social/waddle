import type { Faro } from "@grafana/faro-web-sdk";
import { resetGlobalErrorDedupeForTesting } from "./global-errors";
import { setConfiguredTrustedSpanOrigins, setFaro, setGateZeroFaroScope, type FaroDeploymentScope } from "./runtime";

export function __setFaroForTesting(instance: Faro | null): void {
  setFaro(instance);
  if (!instance) {
    setConfiguredTrustedSpanOrigins(new Set());
    setGateZeroFaroScope(null);
    resetGlobalErrorDedupeForTesting();
  }
}
export function __setGateZeroFaroScopeForTesting(scope: FaroDeploymentScope | null): void {
  setGateZeroFaroScope(scope);
}
