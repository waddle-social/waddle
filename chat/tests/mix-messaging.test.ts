import { describe, test, expect, mock } from "bun:test";
import type { Agent } from "stanza";
import {
  joinMixChannel,
  leaveMixChannel,
  sendMixMessage,
  setMixChannelNick,
} from "../src/lib/xmpp/mix-messaging";
import {
  MIX_NODE_MESSAGES,
  MIX_NODE_PARTICIPANTS,
} from "../src/lib/xmpp/extensions/mix";
import {
  mixChannelBareJidFor,
  mixChannelBareJidForAccountJid,
} from "../src/lib/xmpp/jid";

function makeAgent() {
  return {
    sendIQ: mock(async () => ({})),
    sendMessage: mock(() => undefined),
  } as unknown as Agent & {
    sendIQ: ReturnType<typeof mock>;
    sendMessage: ReturnType<typeof mock>;
  };
}

describe("mix client-join / client-leave (XEP-0405)", () => {
  test("joinMixChannel sends a MIX-PAM client-join with default node set", async () => {
    const xmpp = makeAgent();

    await joinMixChannel(xmpp, "general@mix.waddle.social", "Alice");

    expect(xmpp.sendIQ).toHaveBeenCalledTimes(1);
    expect(xmpp.sendIQ).toHaveBeenCalledWith(
      expect.objectContaining({
        type: "set",
        clientJoin: {
          channel: "general@mix.waddle.social",
          join: {
            nick: "Alice",
            subscribes: [
              { node: MIX_NODE_MESSAGES },
              { node: MIX_NODE_PARTICIPANTS },
            ],
          },
        },
      }),
    );
  });

  test("joinMixChannel honours an explicit node list", async () => {
    const xmpp = makeAgent();

    await joinMixChannel(xmpp, "g@mix.example.com", "B", [
      { node: "urn:xmpp:mix:nodes:info" },
    ]);

    expect(xmpp.sendIQ).toHaveBeenCalledWith(
      expect.objectContaining({
        clientJoin: expect.objectContaining({
          join: expect.objectContaining({
            subscribes: [{ node: "urn:xmpp:mix:nodes:info" }],
          }),
        }),
      }),
    );
  });

  test("leaveMixChannel sends a MIX-PAM client-leave", async () => {
    const xmpp = makeAgent();

    await leaveMixChannel(xmpp, "general@mix.waddle.social");

    expect(xmpp.sendIQ).toHaveBeenCalledWith(
      expect.objectContaining({
        type: "set",
        clientLeave: {
          channel: "general@mix.waddle.social",
          leave: {},
        },
      }),
    );
  });
});

describe("mix setnick (XEP-0369)", () => {
  test("setMixChannelNick targets the channel JID with a setnick payload", async () => {
    const xmpp = makeAgent();

    await setMixChannelNick(xmpp, "g@mix.example.com", "Ally");

    expect(xmpp.sendIQ).toHaveBeenCalledWith(
      expect.objectContaining({
        type: "set",
        to: "g@mix.example.com",
        mixSetnick: { nick: "Ally" },
      }),
    );
  });
});

describe("mix message publish (XEP-0369 §7.1)", () => {
  test("sendMixMessage emits a groupchat-typed message to the channel JID", () => {
    const xmpp = makeAgent();

    const id = sendMixMessage(xmpp, "g@mix.example.com", "hi mix");

    expect(typeof id).toBe("string");
    expect(id.length).toBeGreaterThan(0);
    expect(xmpp.sendMessage).toHaveBeenCalledTimes(1);
    expect(xmpp.sendMessage).toHaveBeenCalledWith(
      expect.objectContaining({
        id,
        to: "g@mix.example.com",
        type: "groupchat",
        body: "hi mix",
      }),
    );
  });

  test("sendMixMessage carries a thread when supplied", () => {
    const xmpp = makeAgent();

    sendMixMessage(xmpp, "g@mix.example.com", "in thread", "thread-42");

    expect(xmpp.sendMessage).toHaveBeenCalledWith(
      expect.objectContaining({
        thread: { id: "thread-42" },
      }),
    );
  });

  test("sendMixMessage honours an explicit msgId", () => {
    const xmpp = makeAgent();

    const id = sendMixMessage(xmpp, "g@mix.example.com", "x", undefined, "fixed-id");

    expect(id).toBe("fixed-id");
    expect(xmpp.sendMessage).toHaveBeenCalledWith(
      expect.objectContaining({ id: "fixed-id" }),
    );
  });
});

describe("mix channel JID helpers", () => {
  test("mixChannelBareJidForAccountJid uses the mix.<domain> subdomain", () => {
    const jid = mixChannelBareJidForAccountJid(
      "alice@waddle.social/web",
      "wad",
      "ch1",
    );
    expect(jid).toBe("wad_ch1@mix.waddle.social");
  });

  test("mixChannelBareJidFor sources domain from the session JID", () => {
    const session = {
      jid: "bob@mix-test.example.com/desktop",
      username: "bob",
    } as unknown as Parameters<typeof mixChannelBareJidFor>[0];
    expect(mixChannelBareJidFor(session, "wad", "ch")).toBe(
      "wad_ch@mix.mix-test.example.com",
    );
  });
});
