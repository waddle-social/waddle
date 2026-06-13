import { describe, expect, test } from "bun:test";
import { readFileSync } from "node:fs";

// Regression: pin/unpin in the chat-app controller must work for
// standalone MUCs (channels without a parent Waddle space). The earlier
// guard required `waddles.currentSpace.value` to be non-null, which
// silently aborted every pin click in a standalone channel — and also
// in any nested channel whose parent space hadn't surfaced yet in the
// `waddles` list. Both paths now resolve the space id off the channel
// itself, matching how every other channel-scoped action (load, send,
// edit, retract, react) addresses its room.

const controllerSource = readFileSync(
  new URL("../src/shell/chat-app-controller.ts", import.meta.url),
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

  test("pin target lookup uses the active timeline for DMs and channels", () => {
    expect(controllerSource).toContain(
      "activeTarget.value.messages.value.find((m) => m.id === messageId)",
    );
    expect(controllerSource).not.toContain(
      "messaging.messages.value.find((m) => m.id === messageId)",
    );
  });
});
