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

function block(name: string): string {
  const start = controllerSource.indexOf(`function ${name}(`);
  if (start === -1) throw new Error(`function ${name} not found in chat-app-controller.ts`);
  let depth = 0;
  let inBody = false;
  for (let i = start; i < controllerSource.length; i++) {
    const ch = controllerSource[i];
    if (ch === "{") {
      depth++;
      inBody = true;
    } else if (ch === "}") {
      depth--;
      if (inBody && depth === 0) return controllerSource.slice(start, i + 1);
    }
  }
  throw new Error(`could not extract body of ${name}`);
}

describe("pin/unpin handlers for standalone channels", () => {
  for (const name of ["pinActiveMessage", "unpinActiveMessage"] as const) {
    test(`${name} does not require currentSpace`, () => {
      const body = block(name);
      expect(body).not.toContain("currentSpace");
    });

    test(`${name} reads the space id off the active channel`, () => {
      const body = block(name);
      expect(body).toContain("channel.spaceId");
    });
  }
});
