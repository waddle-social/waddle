import { describe, test, expect } from "bun:test";
import type { ReceivedMessage } from "stanza/protocol";
import { buildMessageDispatcher } from "../src/lib/xmpp/message-dispatch";
import type { GroupchatHandlers } from "../src/lib/xmpp/message-parsing";
import type { DmHandlers } from "../src/lib/xmpp/dm-parsing";
import type {
  DmChatStateEvent,
  DmDisplayedEvent,
  DmReactionEvent,
  LiveDmMessage,
  LiveRoomMessage,
  ReactionEvent,
  RoomActivityEvent,
  RoomChatStateEvent,
  RoomDisplayedEvent,
} from "../src/lib/xmpp/types";

type GroupchatCapture = {
  messages: LiveRoomMessage[];
  reactions: ReactionEvent[];
  chatStates: RoomChatStateEvent[];
  displayed: RoomDisplayedEvent[];
  activity: RoomActivityEvent[];
};

type DmCapture = {
  messages: LiveDmMessage[];
  reactions: DmReactionEvent[];
  chatStates: DmChatStateEvent[];
  displayed: DmDisplayedEvent[];
};

function makeDispatcher() {
  const groupchat: GroupchatCapture = {
    messages: [],
    reactions: [],
    chatStates: [],
    displayed: [],
    activity: [],
  };
  const dm: DmCapture = {
    messages: [],
    reactions: [],
    chatStates: [],
    displayed: [],
  };

  const groupchatHandlers = (): GroupchatHandlers => ({
    currentRoom: "room@muc.example.com",
    selfNick: "alice",
    onMessage: (msg) => groupchat.messages.push(msg),
    onChatState: (event) => groupchat.chatStates.push(event),
    onDisplayed: (event) => groupchat.displayed.push(event),
    onReaction: (event) => groupchat.reactions.push(event),
    onActivity: (event) => groupchat.activity.push(event),
  });

  const dmHandlers = (): DmHandlers => ({
    selfBareJid: "alice@example.com",
    onMessage: (msg) => dm.messages.push(msg),
    onChatState: (event) => dm.chatStates.push(event),
    onDisplayed: (event) => dm.displayed.push(event),
    onReaction: (event) => dm.reactions.push(event),
  });

  const dispatch = buildMessageDispatcher(groupchatHandlers, dmHandlers);

  return { dispatch, groupchat, dm };
}

describe("buildMessageDispatcher", () => {
  // Regression: the stanzajs `groupchat` / `chat` events only fire when the
  // incoming message carries a body or link. Body-less reaction stanzas never
  // trip those events — listening on `groupchat`/`chat` silently loses every
  // reaction, chat-state, and delivery marker. This suite guards against a
  // regression to that wiring.

  test("routes body-less groupchat reactions to the room reaction handler", () => {
    const { dispatch, groupchat } = makeDispatcher();

    dispatch({
      from: "room@muc.example.com/bob",
      to: "alice@example.com/web",
      type: "groupchat",
      id: "stanza-1",
      reactions: { id: "target-msg-id", items: ["👍"] },
    } as unknown as ReceivedMessage);

    expect(groupchat.reactions).toHaveLength(1);
    expect(groupchat.reactions[0]).toEqual({
      roomJid: "room@muc.example.com",
      nick: "bob",
      messageId: "target-msg-id",
      emojis: ["👍"],
    });
    expect(groupchat.messages).toHaveLength(0);
  });

  test("routes body-less 1:1 reactions to the DM reaction handler", () => {
    const { dispatch, dm } = makeDispatcher();

    dispatch({
      from: "bob@example.com/web",
      to: "alice@example.com/web",
      type: "chat",
      id: "stanza-1",
      reactions: { id: "target-dm-id", items: ["🎉"] },
    } as unknown as ReceivedMessage);

    expect(dm.reactions).toHaveLength(1);
    expect(dm.reactions[0]).toEqual({
      peerJid: "bob@example.com",
      messageId: "target-dm-id",
      emojis: ["🎉"],
    });
    expect(dm.messages).toHaveLength(0);
  });

  test("still routes body-carrying groupchat messages", () => {
    const { dispatch, groupchat } = makeDispatcher();

    dispatch({
      from: "room@muc.example.com/bob",
      to: "alice@example.com/web",
      type: "groupchat",
      id: "stanza-2",
      body: "hello room",
    } as unknown as ReceivedMessage);

    expect(groupchat.messages).toHaveLength(1);
    expect(groupchat.messages[0].body).toBe("hello room");
  });

  test("skips carbon-wrapped messages (handled by carbon listeners)", () => {
    const { dispatch, dm, groupchat } = makeDispatcher();

    dispatch({
      from: "alice@example.com",
      to: "alice@example.com",
      type: "chat",
      carbon: {
        type: "sent",
        forward: {
          message: {
            from: "alice@example.com/phone",
            to: "bob@example.com",
            type: "chat",
            id: "inner-1",
            body: "from my other device",
          },
        },
      },
    } as unknown as ReceivedMessage);

    expect(dm.messages).toHaveLength(0);
    expect(groupchat.messages).toHaveLength(0);
  });

  test("ignores error-typed messages", () => {
    const { dispatch, dm, groupchat } = makeDispatcher();

    dispatch({
      from: "bob@example.com",
      to: "alice@example.com",
      type: "error",
      id: "err-1",
      body: "bounce",
    } as unknown as ReceivedMessage);

    expect(dm.messages).toHaveLength(0);
    expect(groupchat.messages).toHaveLength(0);
  });

  test("routes body-less groupchat chat-states", () => {
    const { dispatch, groupchat } = makeDispatcher();

    dispatch({
      from: "room@muc.example.com/bob",
      to: "alice@example.com/web",
      type: "groupchat",
      chatState: "composing",
    } as unknown as ReceivedMessage);

    expect(groupchat.chatStates).toHaveLength(1);
    expect(groupchat.chatStates[0].nick).toBe("bob");
    expect(groupchat.chatStates[0].state).toBe("composing");
  });
});
