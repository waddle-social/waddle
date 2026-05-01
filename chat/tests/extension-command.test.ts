import { describe, expect, test } from "bun:test";
import type { ExtensionLaunchDescriptor } from "../src/lib/chat-ui";
import {
  buildExtensionLaunchInvokeIq,
  extensionCommandOutcome,
  extensionCommandFormBlockedReason,
  extensionServiceJidForUserJid,
  invokeExtensionCommand,
  parseExtensionCommandForm,
  parseExtensionCommandLaunches,
  parseExtensionCommandResult,
  submitExtensionCommandForm,
  visibleExtensionCommandFields,
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

  test("builds plain XEP-0050 execute requests for discovered commands", async () => {
    let sent: unknown;
    const xmpp = {
      async sendIQ(iq: unknown) {
        sent = iq;
        return { command: { status: "completed", notes: [] } };
      },
    };

    await invokeExtensionCommand(
      xmpp as any,
      "alice@example.com/web",
      { serviceJid: "extensions.example.com", node: "urn:waddle:extension:noop", name: "Noop" },
    );

    expect(sent).toMatchObject({
      type: "set",
      to: "extensions.example.com",
      command: {
        node: "urn:waddle:extension:noop",
        action: "execute",
      },
    });
    expect((sent as { command?: { form?: unknown } }).command?.form).toBeUndefined();
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

  test("requires core launch identity before invoking action buttons", () => {
    expect(() => buildExtensionLaunchInvokeIq("alice@example.com/web", {
      ...launch,
      pluginId: " ",
    })).toThrow("Launch is missing plugin id.");
  });

  test("requires signed launch proof before invoking action buttons", () => {
    expect(() => buildExtensionLaunchInvokeIq("alice@example.com/web", {
      ...launch,
      launchToken: undefined,
    })).toThrow("Launch is missing launch token.");
  });

  test("builds launch invoke requests without source stanza metadata", () => {
    const iq = buildExtensionLaunchInvokeIq("alice@example.com/web", {
      id: "ask-followup",
      pluginId: "ai-chatbot",
      actionId: "ask-followup",
      commandNode: "urn:waddle:extension:1:invoke",
      launchToken: "signed-followup-token",
      expiresAt: "2026-05-01T10:53:23Z",
      label: "Ask follow-up",
      context: {
        waddleId: "alice@example.com/web",
      },
      payloads: [{
        namespace: "urn:waddle:ai-chatbot:1",
        name: "assistant-followup",
        attributes: {
          xmlns: "urn:waddle:ai-chatbot:1",
          question: "Ask me from chat with /ask.",
        },
        text: "Ask me from chat with /ask.",
        children: [],
      }],
    });

    expect(iq.command.form.fields).toContainEqual({ name: "launch-token", value: "signed-followup-token" });
    expect(iq.command.form.fields).toContainEqual({ name: "expires-at", value: "2026-05-01T10:53:23Z" });
    expect(iq.command.form.fields).toContainEqual({ name: "payload#assistant-followup#question", value: "Ask me from chat with /ask." });
    expect(iq.command.form.fields).not.toContainEqual(expect.objectContaining({ name: "source-stanza-id" }));
    expect(iq.command.form.fields).not.toContainEqual(expect.objectContaining({ name: "waddle#message_stanza_id" }));
  });

  test("parses AI chatbot result launches without source stanza metadata", () => {
    const actions = parseExtensionCommandLaunches({
      fields: [
        { name: "FORM_TYPE", type: "hidden", value: "urn:waddle:extension:1:result" },
        { name: "launch-count", value: "1" },
        { name: "launch#0#id", value: "ask-followup" },
        { name: "launch#0#plugin", value: "ai-chatbot" },
        { name: "launch#0#action", value: "ask-followup" },
        { name: "launch#0#command-node", value: "urn:waddle:extension:1:invoke" },
        { name: "launch#0#label", value: "Ask follow-up" },
        { name: "launch#0#waddle-id", value: "alice@example.com/web" },
        { name: "launch#0#token", value: "signed-followup-token" },
        { name: "launch#0#expires-at", value: "2026-05-01T10:53:23Z" },
        { name: "launch#0#payload#0#namespace", value: "urn:waddle:ai-chatbot:1" },
        { name: "launch#0#payload#0#name", value: "assistant-followup" },
        { name: "launch#0#payload#0#text", value: "Ask me from chat with /ask." },
        { name: "launch#0#payload#0#attr#question", value: "Ask me from chat with /ask." },
      ],
    });

    expect(actions).toEqual([{
      label: "Ask follow-up",
      route: "ask-followup",
      launch: {
        id: "ask-followup",
        pluginId: "ai-chatbot",
        actionId: "ask-followup",
        commandNode: "urn:waddle:extension:1:invoke",
        launchToken: "signed-followup-token",
        expiresAt: "2026-05-01T10:53:23Z",
        label: "Ask follow-up",
        context: {
          waddleId: "alice@example.com/web",
        },
        payloads: [{
          namespace: "urn:waddle:ai-chatbot:1",
          name: "assistant-followup",
          attributes: {
            xmlns: "urn:waddle:ai-chatbot:1",
            question: "Ask me from chat with /ask.",
          },
          text: "Ask me from chat with /ask.",
          children: [],
        }],
      },
    }]);
  });

  test("does not parse launch actions from unrelated data forms", () => {
    expect(parseExtensionCommandLaunches({
      fields: [
        { name: "FORM_TYPE", type: "hidden", value: "urn:example:other:result" },
        { name: "launch-count", value: "1" },
        { name: "launch#0#id", value: "ask-followup" },
        { name: "launch#0#plugin", value: "ai-chatbot" },
        { name: "launch#0#action", value: "ask-followup" },
        { name: "launch#0#command-node", value: "urn:waddle:extension:1:invoke" },
        { name: "launch#0#label", value: "Ask follow-up" },
        { name: "launch#0#waddle-id", value: "alice@example.com/web" },
        { name: "launch#0#token", value: "signed-followup-token" },
        { name: "launch#0#expires-at", value: "2026-05-01T10:53:23Z" },
      ],
    })).toEqual([]);
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
    expect(extensionCommandOutcome({
      status: "executing",
      form: { fields: [{ name: "payload#question", type: "text-single", value: "" }] },
      notes: [],
    })).toEqual({ state: "warning" });

    expect(extensionCommandOutcome({
      status: "completed",
      form: {
        fields: [
          { name: "FORM_TYPE", type: "hidden", value: "urn:waddle:extension:1:result" },
          { name: "extension#body", value: "I can help summarize the recent thread." },
          { name: "extension#prompt", value: "Morning" },
        ],
      },
      notes: [{ type: "info", value: "Produced 1 message enrichment" }],
    })).toEqual({
      state: "success",
      detail: "I can help summarize the recent thread. Morning",
    });
  });

  test("parses XEP-0004 form fields, options, visibility, and forbidden fields", () => {
    const fields = parseExtensionCommandForm({
      fields: [
        { type: "fixed", value: "Section heading" },
        { name: "FORM_TYPE", type: "hidden", value: "urn:waddle:extension:1" },
        { name: "hidden#multi", type: "hidden", value: ["a", "b"] },
        { name: "payload#question", type: "text-single", label: "Question", required: true, desc: "Ask the room." },
        {
          name: "payload#mode",
          type: "list-single",
          label: "Mode",
          value: "single",
          options: [
            { label: "Single choice", value: "single" },
            { label: "Multiple choice", value: "multi" },
          ],
        },
        { name: "payload#notify", type: "boolean", value: true },
        { name: "waddle#api_key", type: "text-private", label: "API key" },
      ],
    });

    expect(visibleExtensionCommandFields(fields).map((field) => field.name)).toEqual([
      "fixed:Section heading",
      "payload#question",
      "payload#mode",
      "payload#notify",
      "waddle#api_key",
    ]);
    expect(fields[0]).toMatchObject({
      label: "fixed:Section heading",
      type: "fixed",
      value: "Section heading",
    });
    expect(fields[3]).toMatchObject({
      label: "Question",
      required: true,
      description: "Ask the room.",
      value: "",
      blocked: false,
    });
    expect(fields[4]?.options).toEqual([
      { label: "Single choice", value: "single" },
      { label: "Multiple choice", value: "multi" },
    ]);
    expect(fields[5]?.value).toBe("true");
    expect(extensionCommandFormBlockedReason(fields)).toBe("Extension command form contains a forbidden field: API key.");
  });

  test("parses allowed XEP-0050 stage actions", () => {
    const result = parseExtensionCommandResult({
      command: {
        status: "executing",
        sid: "session-1",
        availableActions: { execute: "next", next: true },
        notes: [],
      },
    });

    expect(result.sessionId).toBe("session-1");
    expect(result.actions).toEqual({
      execute: "next",
      allowed: ["next", "cancel"],
    });
  });

  test("defaults executing commands without actions to complete plus cancel", () => {
    const result = parseExtensionCommandResult({
      command: {
        status: "executing",
        sid: "session-1",
        notes: [],
      },
    });

    expect(result.actions).toEqual({
      allowed: ["complete", "cancel"],
    });
  });

  test("preserves XEP-0004 multi-value fields from Stanza form values", () => {
    const fields = parseExtensionCommandForm({
      fields: [
        { name: "payload#choices", type: "list-multi", value: ["yes", "maybe"] },
        { name: "payload#notes", type: "text-multi", value: ["first", "second"] },
        { name: "payload#notify", type: "boolean" },
      ],
    });

    expect(fields[0]?.values).toEqual(["yes", "maybe"]);
    expect(fields[1]?.values).toEqual(["first", "second"]);
    expect(fields[2]).toMatchObject({ value: "0", values: ["0"] });
  });

  test("submits XEP-0050 session id and XEP-0004 multi values using Stanza names", async () => {
    let sent: unknown;
    const xmpp = {
      async sendIQ(iq: unknown) {
        sent = iq;
        return { command: { status: "completed", notes: [] } };
      },
    };

    await submitExtensionCommandForm(
      xmpp as any,
      { serviceJid: "extensions.example.com", node: "urn:waddle:extension:poll", name: "Poll" },
      "session-1",
      [
        {
          name: "fixed:Do not send me",
          label: "Do not send me",
          type: "fixed",
          value: "Do not send me",
          values: ["Do not send me"],
          options: [],
          required: false,
          blocked: false,
          hidden: false,
        },
        {
          name: "payload#choices",
          label: "Choices",
          type: "list-multi",
          value: "yes",
          values: ["yes", "maybe"],
          options: [],
          required: true,
          blocked: false,
          hidden: false,
        },
        {
          name: "payload#notify",
          label: "Notify",
          type: "boolean",
          value: "0",
          values: ["0"],
          options: [],
          required: true,
          blocked: false,
          hidden: false,
        },
        {
          name: "hidden#multi",
          label: "Hidden",
          type: "hidden",
          value: "alpha",
          values: ["alpha", "beta"],
          options: [],
          required: false,
          blocked: false,
          hidden: true,
        },
      ],
      "next",
    );

    expect(sent).toMatchObject({
      type: "set",
      to: "extensions.example.com",
      command: {
        node: "urn:waddle:extension:poll",
        sid: "session-1",
        action: "next",
        form: {
          type: "submit",
          fields: [
            { name: "payload#choices", type: "list-multi", value: ["yes", "maybe"] },
            { name: "payload#notify", type: "boolean", value: false },
            { name: "hidden#multi", type: "hidden", value: ["alpha", "beta"] },
          ],
        },
      },
    });
  });
});
