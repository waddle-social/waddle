import { describe, expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { renderVueComponent } from "./helpers/render-vue-sfc";

function surfaceSource(name: string): string {
  return readFileSync(
    new URL(`../src/components/calls/${name}`, import.meta.url),
    "utf8",
  );
}

describe("CallControls participants toggle", () => {
  test("renders a Participants toggle with the attendee count instead of a volume button", async () => {
    const html = await renderVueComponent(
      "../src/components/calls/CallControls.vue",
      {
        micEnabled: true,
        camEnabled: true,
        screenShareEnabled: false,
        screenShareSupported: false,
        isExpanded: true,
        participantsOpen: false,
        participantCount: 3,
        chatOpen: false,
        chatUnread: 0,
        viewMode: "gallery",
        selfViewHidden: false,
      },
      import.meta.url,
    );

    expect(html).toContain('aria-label="Open participants"');
    expect(html).toContain(">3<");
    expect(html).not.toContain("volume mixer");
  });

  test("drops the old volume button and wires the participants emit", () => {
    const source = surfaceSource("CallControls.vue");
    expect(source).toContain("toggleParticipants");
    expect(source).toContain("participantsOpen");
    expect(source).toContain("participantCount");
    expect(source).not.toContain("toggleVolume");
    expect(source).not.toContain("volumeOpen");
  });
});

describe("ContentArea feeds the in-call Chat tab", () => {
  function contentAreaSource(): string {
    return readFileSync(
      new URL("../src/components/chat/ContentArea.vue", import.meta.url),
      "utf8",
    );
  }

  test("passes the call-thread message slice + author maps into the Expanded surface", () => {
    const source = contentAreaSource();
    expect(source).toContain("callChatMessages");
    expect(source).toContain(":call-chat-messages=\"callChatMessages\"");
    expect(source).toContain(":avatar-url-by-author=\"avatarUrlByAuthor\"");
  });

  test("drives the unread sync from inbound call-thread messages and Chat-tab focus", () => {
    const source = contentAreaSource();
    expect(source).toContain("inboundCallChatThreadIds");
    expect(source).toContain("isCallChatTabFocused");
    expect(source).toContain("syncCallChatUnread");
  });
});

describe("call surfaces use the Participants dock", () => {
  test("both surfaces drive the roster + dock and drop the volume mixer dialog", () => {
    for (const name of ["CallSplitContainer.vue", "CallExpandedSurface.vue"]) {
      const source = surfaceSource(name);
      expect(source).toContain("useCallRoster");
      expect(source).not.toContain("CallVolumeMixerDialog");
      expect(source).not.toContain("useCallVolumeMixer");
    }
  });

  test("Split's Participants button bumps to Expanded with the dock open", () => {
    const source = surfaceSource("CallSplitContainer.vue");
    expect(source).toContain("openCallDock");
    expect(source).toContain('$callUiMode.set("expanded")');
    expect(source).toContain("@toggle-participants");
  });

  test("Split's Chat button bumps to Expanded with the Chat tab open and shows unread", () => {
    const source = surfaceSource("CallSplitContainer.vue");
    expect(source).toContain('setCallDockTab("chat")');
    expect(source).toContain("@toggle-chat");
    // The unread badge rides along even in Split so it's visible before expanding.
    expect(source).toContain("$callChatUnread");
    expect(source).toContain(":chat-unread");
  });

  test("Expanded toggles the dock, renders it gated on open, and reflows the stage", () => {
    const source = surfaceSource("CallExpandedSurface.vue");
    expect(source).toContain("$callDockOpen");
    expect(source).toContain("@toggle-participants=\"toggleCallParticipants\"");
    // The dock renders only when open, as a sibling of the grid inside
    // __main (a flex row), so the stage reflows beside it.
    expect(source).toContain("<CallDock");
    expect(source).toContain("v-if=\"dockOpen\"");
  });

  test("Expanded drives the dock's active tab + chat unread and projects the Chat panel", () => {
    const source = surfaceSource("CallExpandedSurface.vue");
    // The dock's selected tab and the chat unread badge are driven from stores.
    expect(source).toContain("$callDockTab");
    expect(source).toContain("$callChatUnread");
    expect(source).toContain(":active-tab");
    expect(source).toContain(":chat-unread");
    expect(source).toContain("@set-tab");
    // The Chat panel is projected into the dock's chat slot, and told when it's
    // the visible tab so it can re-pin to the newest message.
    expect(source).toContain("<CallChatPanel");
    expect(source).toContain("#chat");
    expect(source).toContain(":visible=\"chatOpen\"");
  });

  test("Expanded wires the control-bar Chat toggle and drops the footer composer", () => {
    const source = surfaceSource("CallExpandedSurface.vue");
    expect(source).toContain("@toggle-chat=\"toggleCallChat\"");
    expect(source).toContain(":chat-open");
    // The standalone footer chat section is gone — chat now lives in the dock.
    expect(source).not.toContain("call-expanded__chat");
  });

  test("Expanded Escape closes the open dock before collapsing the call", () => {
    const source = surfaceSource("CallExpandedSurface.vue");
    const start = source.indexOf("function onKeydown");
    const guard = source.slice(start, source.indexOf("collapseToSplit()", start));
    // The guard must actually close the dock (not merely mention the flag)
    // and short-circuit before the collapse.
    expect(guard).toContain("if (dockOpen.value)");
    expect(guard).toContain("closeCallDock()");
  });

  test("Immersive keeps the stage edge-to-edge and overlays the dock", () => {
    const source = surfaceSource("CallExpandedSurface.vue");
    expect(source).toContain('uiMode.value === "immersive"');
    expect(source).toContain("call-expanded--immersive");
    expect(source).toContain("call-expanded__dock-overlay");
    expect(source).toContain("chromeVisible");
    expect(source).toContain("@pointermove=\"revealImmersiveChrome\"");
    expect(source).toContain("@focusin=\"revealImmersiveChrome\"");
    expect(source).toContain("chromeHasFocus()");
    expect(source).toContain("watch(isImmersive");
    expect(source).toContain("visibility: hidden");
    expect(source).toContain("position: absolute");
  });

  test("Immersive layers browser fullscreen and exits back to Expanded", () => {
    const surface = surfaceSource("CallExpandedSurface.vue");
    expect(surface).toContain("requestFullscreen");
    expect(surface).toContain("document.exitFullscreen()");
    expect(surface).toContain("shouldExitNativeFullscreenForModeChange");
    expect(surface).toContain("document.fullscreenElement === surfaceRef.value");
    expect(surface).toContain("if (document.fullscreenElement) return");
    expect(surface).toContain("const wasNativeFullscreenActive = nativeFullscreenActive.value");
    expect(surface).toContain("if (wasNativeFullscreenActive && !nativeFullscreenActive.value)");
    expect(surface).toContain("fullscreenchange");
    expect(surface).toContain("callUiModeAfterFullscreenExit(uiMode.value)");
    expect(surface).toContain("void toggleExpandedSurface()");
    expect(surface).toContain("@toggle-native-fullscreen");
    expect(surface).toContain("@toggle-immersive");

    const controls = surfaceSource("CallControls.vue");
    expect(controls).toContain("isImmersive");
    expect(controls).toContain("isNativeFullscreen");
    expect(controls).toContain("toggleNativeFullscreen");
    expect(controls).toContain("isNativeFullscreen ? Minimize2 : Maximize2");
    expect(controls).toContain("toggleImmersive");
    expect(controls).not.toContain(":aria-pressed=\"isExpanded\"");
    expect(controls).toContain("Enter browser fullscreen");
    expect(controls).toContain("Exit browser fullscreen");
  });

  test("call lifecycle cleanup resets any non-split UI mode for the next call", () => {
    const source = surfaceSource("CallOverlay.vue");
    expect(source).toContain('$callUiMode.get() !== "split"');
    expect(source).toContain("resetCallUiModeAfterCallEnd()");
  });
});
