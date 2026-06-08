// The global Threads view (`urn:waddle:threads:0`) lists both channel
// (MUC room) threads and DM (partner-keyed) threads. Each row carries a
// bare JID in `entry.channel`. `resolveThreadEntryTarget` classifies that
// JID so the shell can route a click to the right surface: a channel
// selection for MUC rooms, or a DM open for partner JIDs. Misclassifying
// a DM as a channel was the #917 bug — it routed DM threads into
// `selectChannel(<partner-localpart>)`, a nonexistent channel.

import { describe, expect, test } from "bun:test";
import { resolveThreadEntryTarget } from "../src/lib/threads-view-target";

const MUC_DOMAIN = "muc.waddle.example";

describe("resolveThreadEntryTarget", () => {
  test("classifies a listed channel by its exact room JID", () => {
    const channels = [{ id: "general", jid: "general@muc.waddle.example" }];
    expect(
      resolveThreadEntryTarget("general@muc.waddle.example", {
        channels,
        managedMucDomain: MUC_DOMAIN,
      }),
    ).toEqual({ kind: "channel", channelId: "general" });
  });

  test("classifies a managed MUC room not yet in the channel list as a channel", () => {
    expect(
      resolveThreadEntryTarget("orphan@muc.waddle.example", {
        channels: [],
        managedMucDomain: MUC_DOMAIN,
      }),
    ).toEqual({ kind: "channel", channelId: "orphan" });
  });

  test("classifies a partner JID on a non-MUC domain as a DM", () => {
    expect(
      resolveThreadEntryTarget("bob@waddle.example", {
        channels: [{ id: "general", jid: "general@muc.waddle.example" }],
        managedMucDomain: MUC_DOMAIN,
      }),
    ).toEqual({ kind: "dm", peerJid: "bob@waddle.example" });
  });

  test("bare-normalizes a resource-qualified DM JID", () => {
    expect(
      resolveThreadEntryTarget("bob@waddle.example/phone", {
        channels: [],
        managedMucDomain: MUC_DOMAIN,
      }),
    ).toEqual({ kind: "dm", peerJid: "bob@waddle.example" });
  });

  test("routes a DM peer to a DM even when its node matches a channel id", () => {
    // A DM with `general@waddle.example` shares the node `general` with the
    // `general` channel — but it lives on the user domain, not the MUC
    // domain, so it must open as a DM, never as the channel.
    const channels = [{ id: "general", jid: "general@muc.waddle.example" }];
    expect(
      resolveThreadEntryTarget("general@waddle.example", {
        channels,
        managedMucDomain: MUC_DOMAIN,
      }),
    ).toEqual({ kind: "dm", peerJid: "general@waddle.example" });
  });

  test("prefers an explicit channel match over the MUC-domain heuristic", () => {
    // A foreign-domain room that is nonetheless a known channel by exact
    // JID must still classify as that channel.
    const channels = [{ id: "pretty-name", jid: "room@conference.other.example" }];
    expect(
      resolveThreadEntryTarget("room@conference.other.example", {
        channels,
        managedMucDomain: MUC_DOMAIN,
      }),
    ).toEqual({ kind: "channel", channelId: "pretty-name" });
  });

  test("returns null for an unusable JID", () => {
    expect(
      resolveThreadEntryTarget("", { channels: [], managedMucDomain: MUC_DOMAIN }),
    ).toBeNull();
    expect(
      resolveThreadEntryTarget("@muc.waddle.example", {
        channels: [],
        managedMucDomain: MUC_DOMAIN,
      }),
    ).toBeNull();
  });
});
