import { describe, expect, test } from "bun:test";
import type { ExtensionLaunchDescriptor } from "../src/lib/chat-ui";
import {
  buildExtensionLaunchInvokeIq,
  extensionCommandOutcome,
  extensionServiceJidForUserJid,
  parseExtensionCommandResult,
} from "../src/lib/xmpp/extension-commands";

const launch: ExtensionLaunchDescriptor = {
  id: "vote-yes",
  pluginId: "decision-polls",
  actionId: "vote",
  commandNode: "urn:waddle:extension:1:invoke",
  launchToken: "launch-token-vote-yes",
  expiresAt: "2026-04-27T12:00:00Z",
  label: "Vote yes",
  context: {
    waddleId: "waddle-123",
    roomJid: "pub@muc.example.com",
    stanzaId: "archive-id-poll-1",
  },
  payloads: [{
    namespace: "urn:waddle:decision-polls:1",
    name: "vote-request",
    attributes: {
      xmlns: "urn:waddle:decision-polls:1",
      "poll-id": "poll-1",
      "option-id": "yes",
    },
    children: [],
  }],
};

describe("extension command invocation", () => {
  test("targets the user's extension service", () => {
    expect(extensionServiceJidForUserJid("alice@example.com/web")).toBe("extensions.example.com");
  });

  test("builds a XEP-0050 invoke request from launch metadata", () => {
    const iq = buildExtensionLaunchInvokeIq("alice@example.com/web", launch);
    expect(iq.to).toBe("extensions.example.com");
    expect(iq.type).toBe("set");
    expect(iq.command.node).toBe("urn:waddle:extension:1:invoke");
    expect(iq.command.action).toBe("execute");
    expect(iq.command.form.type).toBe("submit");
    expect(iq.command.form.fields).toEqual([
      { name: "FORM_TYPE", type: "hidden", value: "urn:waddle:extension:1" },
      { name: "waddle#op", value: "invoke" },
      { name: "plugin", value: "decision-polls" },
      { name: "waddle#room_jid", value: "pub@muc.example.com" },
      { name: "action", value: "vote" },
      { name: "waddle-id", value: "waddle-123" },
      { name: "source-stanza-id", value: "archive-id-poll-1" },
      { name: "launch-id", value: "vote-yes" },
      { name: "launch-token", value: "launch-token-vote-yes" },
      { name: "expires-at", value: "2026-04-27T12:00:00Z" },
      { name: "waddle#waddle_id", value: "waddle-123" },
      { name: "waddle#message_stanza_id", value: "archive-id-poll-1" },
      { name: "waddle#launch_id", value: "vote-yes" },
      { name: "waddle#launch_token", value: "launch-token-vote-yes" },
      { name: "waddle#expires_at", value: "2026-04-27T12:00:00Z" },
      { name: "payload#vote-request#poll-id", value: "poll-1" },
      { name: "payload#vote-request#option-id", value: "yes" },
    ]);
  });

  test("uses source stanza metadata when context only carries the waddle id", () => {
    const iq = buildExtensionLaunchInvokeIq("alice@example.com/web", {
      ...launch,
      context: { waddleId: "waddle-123" },
      source: { stanzaId: "archive-id-from-source", by: "pub@muc.example.com" },
    });
    expect(iq.command.form.fields.find((field) => field.name === "waddle#message_stanza_id")?.value)
      .toBe("archive-id-from-source");
  });

  test("requires launch tokens before invoking action buttons", () => {
    expect(() => buildExtensionLaunchInvokeIq("alice@example.com/web", {
      ...launch,
      launchToken: undefined,
    })).toThrow("Launch is missing launch token.");
  });

  test("classifies command notes and non-completed statuses for button state", () => {
    const warning = parseExtensionCommandResult({
      command: {
        status: "completed",
        notes: [{ type: "warn", value: "Poll is already closed." }],
      },
    });
    expect(extensionCommandOutcome(warning)).toEqual({
      state: "warning",
      detail: "Poll is already closed.",
    });

    const error = parseExtensionCommandResult({
      command: {
        status: "completed",
        notes: [{ type: "error", value: "Launch token expired." }],
      },
    });
    expect(extensionCommandOutcome(error)).toEqual({
      state: "error",
      detail: "Launch token expired.",
    });

    expect(extensionCommandOutcome({ status: "executing", notes: [] })).toEqual({
      state: "warning",
      detail: "Extension returned a form that this client cannot complete yet.",
    });
  });
});
