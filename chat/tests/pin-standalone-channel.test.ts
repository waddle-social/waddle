import { describe, expect, mock, test } from "bun:test";
import { readFileSync } from "node:fs";
import { computed, effectScope, ref } from "vue";
import { usePinnedMessages } from "../src/shell/controllers/use-pinned-messages";
import { useChatShellState } from "../src/shell/state";

// Regression: pin/unpin in the chat-app controller must work for
// standalone MUCs (channels without a parent Waddle space). The earlier
// guard required `waddles.currentSpace.value` to be non-null, which
// silently aborted every pin click in a standalone channel — and also
// in any nested channel whose parent space hadn't surfaced yet in the
// `waddles` list. Both paths now resolve the space id off the channel
// itself, matching how every other channel-scoped action (load, send,
// edit, retract, react) addresses its room.

const controllerSource = readFileSync(
  new URL("../src/shell/controllers/use-pinned-messages.ts", import.meta.url),
  "utf8",
);

describe("pin/unpin handlers for standalone channels", () => {
  test("pinMessage call site reads spaceId off the active channel", () => {
    expect(controllerSource).toContain(
      'client.pinMessage(channel.spaceId ?? "", channel.id',
    );
  });

  test("unpinMessage call site reads spaceId off the active channel", () => {
    expect(controllerSource).toContain(
      'client.unpinMessage(channel.spaceId ?? "", channel.id',
    );
  });

  test("pin call site no longer threads space.id from currentSpace", () => {
    expect(controllerSource).not.toContain("client.pinMessage(space.id");
    expect(controllerSource).not.toContain("client.unpinMessage(space.id");
  });

  test("no pin handler reaches for waddles.currentSpace.value", () => {
    // The full call site is the only place a removed guard would
    // re-appear; pin a narrow regex around the two function names so
    // unrelated currentSpace usages elsewhere in the controller (modals,
    // headers) do not trip the assertion.
    const pinRegion = controllerSource.match(
      /function pinActiveMessage[\s\S]{0,800}/,
    )?.[0] ?? "";
    const unpinRegion = controllerSource.match(
      /function unpinActiveMessage[\s\S]{0,800}/,
    )?.[0] ?? "";
    expect(pinRegion).not.toContain("currentSpace");
    expect(unpinRegion).not.toContain("currentSpace");
  });

  test.each(["channel", "dm", "empty"])("pin actions use only the selected conversation (%s)", async (surface) => {
    const pinMessage = mock(async () => {});
    const pinDirectMessage = mock(async () => {});
    const ensureMessageLoaded = mock(async () => true);
    const scrollToMessage = mock(async () => {});
    const scope = effectScope();
    try {
      const actions = scope.run(() => {
        const ui = useChatShellState();
        ui.sidebarMode.value = surface === "channel" ? "channels" : "dms";
        const messaging = { messages: ref([{ id: "message", reactionTargetId: "room-stanza" }]), ensureMessageLoaded };
        const dmMessaging = { messages: ref([{ id: "message", replyableId: "dm-stanza" }]), ensureMessageLoaded };
        return usePinnedMessages({
          ui,
          xmppClient: computed(() => ({ pinMessage, pinDirectMessage })),
          session: computed(() => null),
          waddles: { currentChannel: ref({ id: "general" }) },
          messaging,
          dmMessaging,
          dmConversations: { activePeerJid: ref(surface === "dm" ? "bob@example.com" : null) },
          isActiveDirectDmSurface: () => surface === "dm",
          activeTarget: computed(() => surface === "empty" ? null : surface === "dm" ? dmMessaging : messaging),
          contentAreaRef: ref({ scrollToMessage }),
        } as never);
      })!;
      actions.pinActiveMessage("message");
      await actions.jumpToPinnedMessage("stanza");
      expect(pinMessage.mock.calls).toEqual(surface === "channel" ? [["", "general", "room-stanza"]] : []);
      expect(pinDirectMessage.mock.calls).toEqual(surface === "dm" ? [["bob@example.com", "dm-stanza"]] : []);
      expect(ensureMessageLoaded.mock.calls).toEqual(surface === "empty" ? [] : [["stanza"]]);
      expect(scrollToMessage.mock.calls).toEqual(surface === "empty" ? [] : [["stanza"]]);
    } finally {
      scope.stop();
    }
  });
});
