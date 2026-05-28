/// Validation of the XEP-0050 `register-device` stage-4 result the WASM
/// client surfaces. Extracted from `client.ts` so the guard can be
/// unit-tested without standing up the whole WASM + transport surface.

export interface RegisterDeviceResult {
  node: string;
  deviceId: string;
}

/// Parse the raw `register_push_device` result into the typed
/// `(node, deviceId)` pair the chat persists.
///
/// BOTH fields MUST be present and non-empty. A missing/empty
/// `deviceId` would force the per-device `disable-device` opt-out into
/// disable-everywhere semantics that take down sibling devices (Apple,
/// Android, other browsers) registered under the same XEP-0357 node.
/// Returns `null` for any malformed shape so the caller refuses to
/// persist a half-populated record.
export function parseRegisterDeviceResult(result: unknown): RegisterDeviceResult | null {
  if (!result || typeof result !== "object") return null;
  const candidate = result as { node?: unknown; deviceId?: unknown };
  if (
    typeof candidate.node !== "string" ||
    candidate.node.length === 0 ||
    typeof candidate.deviceId !== "string" ||
    candidate.deviceId.length === 0
  ) {
    return null;
  }
  return { node: candidate.node, deviceId: candidate.deviceId };
}
