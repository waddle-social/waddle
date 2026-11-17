import { describe, expect, test } from "bun:test";
import {
  canonicalDomain,
  canonicalRoomJid,
  domainKind,
  isHomeDomain,
  isRemoteRoomJid,
  parseCommunityAddress,
  parseSpaceNodeIri,
  spaceNodeIri,
} from "../src/lib/xmpp/remote-address";
import { channelIdForRoom } from "../src/lib/xmpp/discovery";
import { bareJidKey, roomBareJidFor } from "../src/lib/xmpp/jid";
import type { WaddleSession } from "../src/lib/server-auth";

const HOME = { accountDomain: "home.test", mucService: "muc.home.test", spacesService: "spaces.home.test" };

describe("community address entry", () => {
  test("a bare room JID is a room, in canonical form", () => {
    expect(parseCommunityAddress("  General@Chat.Example.ORG. ")).toEqual({
      ok: true,
      address: { kind: "room", roomJid: "general@chat.example.org" },
    });
  });

  test("an xmpp: join URI is a room", () => {
    expect(parseCommunityAddress("xmpp:dev@chat.example.org?join")).toEqual({
      ok: true,
      address: { kind: "room", roomJid: "dev@chat.example.org" },
    });
  });

  test("a node IRI is a Space, with the node kept as written", () => {
    expect(parseCommunityAddress("xmpp:spaces.example.org?;node=Team%20A")).toEqual({
      ok: true,
      address: { kind: "space", serviceJid: "spaces.example.org", node: "Team A" },
    });
    expect(parseCommunityAddress("xmpp:pubsub.example.org?pubsub;action=subscribe;node=dev")).toEqual({
      ok: true,
      address: { kind: "space", serviceJid: "pubsub.example.org", node: "dev" },
    });
  });

  test("a domain is a service to list", () => {
    expect(parseCommunityAddress("chat.example.org")).toEqual({
      ok: true,
      address: { kind: "service", serviceJid: "chat.example.org" },
    });
  });

  test("an occupant JID is refused rather than trimmed to its room", () => {
    expect(parseCommunityAddress("dev@chat.example.org/alice")).toEqual({ ok: false, error: "resource" });
  });

  test("empty and malformed input are told apart", () => {
    expect(parseCommunityAddress("   ")).toEqual({ ok: false, error: "empty" });
    expect(parseCommunityAddress("not an address")).toEqual({ ok: false, error: "malformed" });
    expect(parseCommunityAddress("xmpp:")).toEqual({ ok: false, error: "malformed" });
  });

  test("domains the server refuses for federation are refused before any request", () => {
    for (const input of ["room@127.0.0.1", "room@[::1]", "room@2130706433", "room@0x7f.1", "room@127.1", "localhost", "xmpp:intranet?;node=x"]) {
      expect(parseCommunityAddress(input)).toEqual({ ok: false, error: "refused-domain" });
    }
  });
});

describe("canonical domains", () => {
  test("one trailing dot is removed, two are invalid", () => {
    expect(canonicalDomain("Example.ORG.")).toBe("example.org");
    expect(canonicalDomain("example.org..")).toBeNull();
  });

  test("an internationalized domain is written as its A-label", () => {
    expect(canonicalDomain("bücher.example")).toBe("xn--bcher-kva.example");
    expect(canonicalRoomJid("room@BÜCHER.example")).toBe("room@xn--bcher-kva.example");
  });

  test("classification follows the server's identity profile", () => {
    expect(domainKind("[::1]")).toBe("ipv6");
    expect(domainKind("127.0.0.1")).toBe("ipv4");
    expect(domainKind("example.123")).toBe("numeric-ipv4");
    expect(domainKind("0x7f.1")).toBe("numeric-ipv4");
    expect(domainKind("localhost")).toBe("single-label");
    expect(domainKind("0x7f.example")).toBe("dns");
  });
});

describe("Space node IRIs", () => {
  test("round-trip a service and node", () => {
    const iri = spaceNodeIri("spaces.example.org", "general & ops");
    expect(parseSpaceNodeIri(iri)).toEqual({ serviceJid: "spaces.example.org", node: "general & ops" });
  });

  test("a value without a service names no Space", () => {
    expect(parseSpaceNodeIri("node=main")).toBeNull();
    expect(parseSpaceNodeIri("xmpp:room@chat.example.org?;node=main")).toBeNull();
    expect(parseSpaceNodeIri(null)).toBeNull();
  });
});

describe("home and remote addresses", () => {
  test("the account domain and discovered services are home", () => {
    expect(isHomeDomain("HOME.test.", HOME)).toBe(true);
    expect(isHomeDomain("muc.home.test", HOME)).toBe(true);
    expect(isHomeDomain("spaces.home.test", HOME)).toBe(true);
    expect(isHomeDomain("muc.other.test", HOME)).toBe(false);
  });

  test("a room is remote when its service is not one of the home's", () => {
    expect(isRemoteRoomJid("general@muc.home.test", HOME)).toBe(false);
    expect(isRemoteRoomJid("general@muc.other.test", HOME)).toBe(true);
  });
});

describe("room ids across servers", () => {
  test("a home room keeps its localpart and a remote room takes its JID", () => {
    expect(channelIdForRoom("general@muc.home.test", "muc.home.test")).toBe("general");
    expect(channelIdForRoom("General@MUC.Other.test", "muc.home.test")).toBe("general@muc.other.test");
  });

  test("two rooms called general on two servers never share an id", () => {
    const local = channelIdForRoom("general@muc.home.test", "muc.home.test");
    const remote = channelIdForRoom("general@muc.other.test", "muc.home.test");
    expect(local).not.toBe(remote);
  });

  test("a qualified id resolves to its own room, not to a home room", () => {
    const session = { jid: "alice@home.test" } as WaddleSession;
    expect(roomBareJidFor(session, "general@muc.other.test")).toBe("general@muc.other.test");
    expect(roomBareJidFor(session, "general")).toBe("general@muc.home.test");
  });

  test("bare JID keys ignore one trailing dot on the domain", () => {
    expect(bareJidKey("room@Muc.Example.org./nick")).toBe(bareJidKey("room@muc.example.org"));
  });
});
