import { describe, expect, test } from "bun:test";
import {
  coerceExternalServices,
  iceServerBundleFromExternalServices,
  iceServersFromExternalServices,
} from "../src/lib/calls/ice-servers";
import type { ExternalService } from "../src/lib/calls/types";

function stun(over: Partial<ExternalService> = {}): ExternalService {
  return {
    serviceType: "stun",
    host: "turn.waddle.social",
    port: 3478,
    transport: "udp",
    restricted: false,
    ...over,
  };
}

function turns(over: Partial<ExternalService> = {}): ExternalService {
  return {
    serviceType: "turns",
    host: "turn.waddle.social",
    port: 443,
    transport: "tcp",
    username: "1700000000:alice@waddle.social/desktop",
    password: "base64hmac==",
    expires: "2026-06-17T12:00:00Z",
    restricted: true,
    ...over,
  };
}

describe("iceServersFromExternalServices", () => {
  test("maps a STUN entry to a stun: URL with no credentials", () => {
    const [server, ...rest] = iceServersFromExternalServices([stun()]);
    expect(rest).toHaveLength(0);
    expect(server.urls).toBe("stun:turn.waddle.social:3478");
    expect(server.username).toBeUndefined();
    expect(server.credential).toBeUndefined();
  });

  test("maps a TLS-TURN entry to a turns: URL with credentials and transport", () => {
    const [server] = iceServersFromExternalServices([turns()]);
    expect(server.urls).toBe("turns:turn.waddle.social:443?transport=tcp");
    expect(server.username).toBe("1700000000:alice@waddle.social/desktop");
    expect(server.credential).toBe("base64hmac==");
  });

  test("maps a plain TURN entry with udp transport", () => {
    const [server] = iceServersFromExternalServices([
      turns({ serviceType: "turn", port: 3478, transport: "udp" }),
    ]);
    expect(server.urls).toBe("turn:turn.waddle.social:3478?transport=udp");
    expect(server.credential).toBe("base64hmac==");
  });

  test("omits the transport query when transport is absent", () => {
    const [server] = iceServersFromExternalServices([
      turns({ serviceType: "turn", transport: undefined }),
    ]);
    expect(server.urls).toBe("turn:turn.waddle.social:443");
  });

  test("skips a TURN entry missing credentials (unusable as a relay)", () => {
    const servers = iceServersFromExternalServices([
      turns({ username: undefined }),
      turns({ password: undefined }),
    ]);
    expect(servers).toHaveLength(0);
  });

  test("preserves order across a mixed server list", () => {
    const servers = iceServersFromExternalServices([turns(), stun()]);
    expect(servers.map((s) => s.urls)).toEqual([
      "turns:turn.waddle.social:443?transport=tcp",
      "stun:turn.waddle.social:3478",
    ]);
  });

  test("returns an empty array for empty input", () => {
    expect(iceServersFromExternalServices([])).toEqual([]);
  });

  test("brackets IPv6 literal hosts in the URI", () => {
    const [turnsServer] = iceServersFromExternalServices([turns({ host: "2001:db8::1" })]);
    expect(turnsServer.urls).toBe("turns:[2001:db8::1]:443?transport=tcp");
    const [stunServer] = iceServersFromExternalServices([stun({ host: "2001:db8::1" })]);
    expect(stunServer.urls).toBe("stun:[2001:db8::1]:3478");
  });

  test("omits the port from the URI when the service has none", () => {
    const [stunServer] = iceServersFromExternalServices([stun({ port: undefined })]);
    expect(stunServer.urls).toBe("stun:turn.waddle.social");
    const [turnsServer] = iceServersFromExternalServices([turns({ port: undefined })]);
    expect(turnsServer.urls).toBe("turns:turn.waddle.social?transport=tcp");
  });
});

describe("iceServerBundleFromExternalServices", () => {
  test("parses XEP-0215 xs:dateTime expiries and chooses the earliest TURN credential", () => {
    const bundle = iceServerBundleFromExternalServices([
      stun({ expires: "2025-01-01T00:00:00Z" }),
      turns({ expires: "2026-06-17T12:05:00Z" }),
      turns({ expires: "2026-06-17T12:00:00Z", host: "turn-two.waddle.test" }),
    ]);

    expect(bundle.servers).toHaveLength(3);
    expect(bundle.earliestExpiryMs).toBe(Date.parse("2026-06-17T12:00:00Z"));
  });

  test("ignores malformed optional expiries", () => {
    expect(iceServerBundleFromExternalServices([
      turns({ expires: "not-a-date" }),
    ]).earliestExpiryMs).toBeNull();
  });
});

describe("coerceExternalServices", () => {
  test("returns an empty array for non-array input (older bundle / shape drift)", () => {
    expect(coerceExternalServices(null)).toEqual([]);
    expect(coerceExternalServices(undefined)).toEqual([]);
    expect(coerceExternalServices({ serviceType: "stun" })).toEqual([]);
  });

  test("keeps well-formed entries and preserves credentials + restricted", () => {
    const services = coerceExternalServices([
      {
        serviceType: "turns",
        host: "turn.waddle.social",
        port: 443,
        transport: "tcp",
        username: "u",
        password: "p",
        expires: "2026-06-17T12:00:00Z",
        restricted: true,
      },
      { serviceType: "stun", host: "turn.waddle.social", port: 3478, transport: "udp", restricted: false },
    ]);
    expect(services).toHaveLength(2);
    expect(services[0]).toMatchObject({ serviceType: "turns", username: "u", password: "p", restricted: true });
    expect(services[1]).toMatchObject({ serviceType: "stun", port: 3478 });
  });

  test("drops entries with an unknown type or missing host", () => {
    const services = coerceExternalServices([
      { serviceType: "ftp", host: "ftp.waddle.social", port: 21, restricted: false },
      { serviceType: "stun", port: 3478, restricted: false },
      { serviceType: "stun", host: "turn.waddle.social", port: 3478, restricted: false },
    ]);
    expect(services).toHaveLength(1);
    expect(services[0].host).toBe("turn.waddle.social");
  });

  test("drops an ill-typed transport but keeps the entry", () => {
    const [service] = coerceExternalServices([
      { serviceType: "stun", host: "turn.waddle.social", port: 3478, transport: "sctp", restricted: false },
    ]);
    expect(service.transport).toBeUndefined();
  });

  test("keeps an entry with an omitted or ill-typed port (port becomes undefined)", () => {
    const services = coerceExternalServices([
      { serviceType: "stun", host: "a.waddle.social", restricted: false },
      { serviceType: "stun", host: "b.waddle.social", port: "3478", restricted: false },
    ]);
    expect(services).toHaveLength(2);
    expect(services[0].port).toBeUndefined();
    expect(services[1].port).toBeUndefined();
  });
});
