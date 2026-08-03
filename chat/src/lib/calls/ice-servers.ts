import type { ExternalService } from "./types";

export type IceServerBundle = {
  servers: RTCIceServer[];
  /** Earliest valid XEP-0215 TURN/TURNS credential expiry, as epoch ms. */
  earliestExpiryMs: number | null;
};

const SERVICE_TYPES = new Set<ExternalService["serviceType"]>(["stun", "turn", "turns"]);
const TRANSPORTS = new Set<NonNullable<ExternalService["transport"]>>(["udp", "tcp"]);

function optionalString(value: unknown): string | undefined {
  return typeof value === "string" ? value : undefined;
}

/**
 * Validate the untyped value returned across the WASM boundary by
 * `fetch_external_services` into typed [`ExternalService`] entries, dropping
 * anything malformed. Defends against bundle drift (a Rust-side shape change
 * shipping ahead of the chat) the same way `fetchVapidPublicKey` hard-checks
 * its boundary — an entry the chat can't model is skipped, never trusted.
 */
export function coerceExternalServices(raw: unknown): ExternalService[] {
  if (!Array.isArray(raw)) return [];
  const services: ExternalService[] = [];
  for (const entry of raw) {
    if (typeof entry !== "object" || entry === null) continue;
    const candidate = entry as Record<string, unknown>;
    const serviceType = candidate.serviceType;
    if (typeof serviceType !== "string" || !SERVICE_TYPES.has(serviceType as ExternalService["serviceType"])) {
      continue;
    }
    if (typeof candidate.host !== "string") continue;
    // `port` is optional (XEP-0215); accept a number, ignore anything else.
    const port = typeof candidate.port === "number" ? candidate.port : undefined;
    const transport =
      typeof candidate.transport === "string" && TRANSPORTS.has(candidate.transport as never)
        ? (candidate.transport as ExternalService["transport"])
        : undefined;
    services.push({
      serviceType: serviceType as ExternalService["serviceType"],
      host: candidate.host,
      port,
      transport,
      username: optionalString(candidate.username),
      password: optionalString(candidate.password),
      expires: optionalString(candidate.expires),
      restricted: candidate.restricted === true,
    });
  }
  return services;
}

/**
 * Map XEP-0215 external services (from the WASM client's
 * `fetch_external_services`) to the `RTCIceServer[]` LiveKit accepts in its
 * connection `rtcConfig.iceServers`.
 *
 * Per-type URI scheme (RFC 7064/7065):
 *   - `stun`  → `stun:host:port`            (no credentials)
 *   - `turn`  → `turn:host:port[?transport]` (username + credential)
 *   - `turns` → `turns:host:port[?transport]` (TLS-protected TURN)
 *
 * A TURN/TURNS entry without both `username` and `password` is dropped: a relay
 * the client cannot authenticate to is unusable and would only add ICE
 * gathering latency. STUN entries never carry credentials.
 */
export function iceServersFromExternalServices(
  services: ReadonlyArray<ExternalService>,
): RTCIceServer[] {
  return iceServerBundleFromExternalServices(services).servers;
}

/**
 * Map XEP-0215 services and retain the earliest TURN credential deadline.
 * `expires` is `xs:dateTime` in XEP-0215, so `Date.parse` converts the
 * already-coerced string at this boundary; malformed optional values are
 * ignored rather than invalidating otherwise usable ICE servers.
 */
export function iceServerBundleFromExternalServices(
  services: ReadonlyArray<ExternalService>,
): IceServerBundle {
  const iceServers: RTCIceServer[] = [];
  let earliestExpiryMs: number | null = null;
  for (const service of services) {
    const hostPort = formatHostPort(service.host, service.port);
    if (service.serviceType === "stun") {
      // STUN URIs take no `?transport` parameter (RFC 7064) and never carry
      // credentials.
      iceServers.push({ urls: `stun:${hostPort}` });
      continue;
    }
    // turn / turns require credentials to be usable as a relay.
    if (service.username === undefined || service.password === undefined) {
      continue;
    }
    // Only services that actually joined the bundle contribute a deadline:
    // a discarded credential-less entry's (possibly already-past) expires
    // must not make the refresher treat every otherwise-valid bundle as
    // expired and spin on retries.
    const expiryMs = service.expires === undefined ? Number.NaN : Date.parse(service.expires);
    if (Number.isFinite(expiryMs)) {
      earliestExpiryMs = earliestExpiryMs === null
        ? expiryMs
        : Math.min(earliestExpiryMs, expiryMs);
    }
    // The `?transport` parameter is turn-specific (RFC 7065).
    const transportQuery = service.transport ? `?transport=${service.transport}` : "";
    const urls = `${service.serviceType}:${hostPort}${transportQuery}`;
    iceServers.push({ urls, username: service.username, credential: service.password });
  }
  return { servers: iceServers, earliestExpiryMs };
}

/**
 * Build the `host[:port]` portion of an ICE URI. IPv6 literals (which contain
 * `:`) MUST be bracketed (RFC 3986 / RFC 7064) so the port colon is
 * unambiguous; a missing port is omitted so the scheme default applies.
 */
function formatHostPort(host: string, port?: number): string {
  const bracketed = host.includes(":") ? `[${host}]` : host;
  return port === undefined ? bracketed : `${bracketed}:${port}`;
}
