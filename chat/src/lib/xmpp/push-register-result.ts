/// Validation of the XEP-0050 `register-device` stage-4 result the WASM
/// client surfaces. Extracted from `client.ts` so the guard can be
/// unit-tested without standing up the whole WASM + transport surface.

export interface RegisterDeviceResult {
  node: string;
  deviceId: string;
}

export interface RegisterPushDeviceRejection {
  code: string;
  message: string;
}

export function parseRegisterPushDeviceRejection(error: unknown): RegisterPushDeviceRejection | null {
  if (!error || typeof error !== "object") return null;
  const candidate = error as { code?: unknown; message?: unknown };
  if (typeof candidate.code !== "string" || typeof candidate.message !== "string") {
    return null;
  }
  return { code: candidate.code, message: candidate.message };
}

export async function retryRegisterPushDeviceAfterSessionExpired<T>(
  register: () => Promise<T | null>,
): Promise<T | null> {
  try {
    return await register();
  } catch (error) {
    // Only the structured session-expired rejection is retryable —
    // any other exception (e.g. a transient connection failure BEFORE
    // the WASM call) must keep propagating exactly as it did before
    // this helper existed, so the caller's persisted node/deviceId
    // are not cleared over a blip.
    const rejection = parseRegisterPushDeviceRejection(error);
    if (rejection?.code !== "session-expired") throw error;
  }

  // Single retry from stage 1 with a fresh XEP-0050 session. A SECOND
  // session-expired is a terminal registration failure (the caller's
  // null-path clears the persisted ids, matching the pre-retry
  // behavior for terminal failures); anything else propagates.
  try {
    return await register();
  } catch (error) {
    const rejection = parseRegisterPushDeviceRejection(error);
    if (rejection?.code === "session-expired") return null;
    throw error;
  }
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
