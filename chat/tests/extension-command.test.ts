import { describe, expect, test } from "bun:test";
import type { ExtensionLaunchDescriptor } from "../src/lib/chat-ui";
import {
  buildExtensionLaunchInvokeIq,
  discoverExtensionCommands,
  discoverExtensionRoutes,
  extensionCommandOutcome,
  extensionCommandFormBlockedReason,
  extensionServiceJidForUserJid,
  fetchExtensionRouteItems,
  invokeExtensionCommand,
  parseExtensionCommandForm,
  parseExtensionCommandResult,
  resolveExtensionRouteStateNode,
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

  test("reads command scope from XEP-0128 disco metadata when discovering commands", async () => {
    const xmpp = {
      async getDiscoItems(jid: string, node?: string) {
        if (jid === "example.com" && node === undefined) {
          return { items: [{ jid: "extensions.example.com", name: "Extensions" }] };
        }
        expect(jid).toBe("extensions.example.com");
        expect(node).toBe("http://jabber.org/protocol/commands");
        return {
          items: [
            { jid, node: "urn:waddle:extension:1:decision-polls", name: "Create Decision Poll" },
            { jid, node: "urn:waddle:extension:1:ai-chatbot", name: "Ask AI Chatbot" },
          ],
        };
      },
      async getDiscoInfo(jid: string, node?: string) {
        expect(jid).toBe("extensions.example.com");
        if (node === undefined) {
          return { features: ["urn:waddle:extension:1", "http://jabber.org/protocol/commands"] };
        }
        if (node === "urn:waddle:extension:1:decision-polls") {
          return {
            extensions: [{
              fields: [
                { name: "FORM_TYPE", value: "urn:waddle:extension:1:command" },
                { name: "waddle#plugin_id", value: "decision-polls" },
                { name: "waddle#command_node", value: "urn:waddle:extension:1:decision-polls" },
                { name: "waddle#command_label", value: "Create Decision Poll" },
                { name: "waddle#command_scope", value: "channel" },
              ],
            }],
          };
        }
        expect(node).toBe("urn:waddle:extension:1:ai-chatbot");
        return {
          extensions: [{
            fields: [
              { name: "FORM_TYPE", value: "urn:waddle:extension:1:command" },
              { name: "waddle#plugin_id", value: "ai-chatbot" },
              { name: "waddle#command_node", value: "urn:waddle:extension:1:ai-chatbot" },
              { name: "waddle#command_label", value: "Ask AI Chatbot" },
              { name: "waddle#command_scope", value: "global" },
            ],
          }],
        };
      },
    };

    const commands = await discoverExtensionCommands(xmpp as any, "alice@example.com/web");
    expect(commands).toEqual([
      {
        serviceJid: "extensions.example.com",
        node: "urn:waddle:extension:1:decision-polls",
        name: "Create Decision Poll",
        scope: "channel",
      },
      {
        serviceJid: "extensions.example.com",
        node: "urn:waddle:extension:1:ai-chatbot",
        name: "Ask AI Chatbot",
        scope: "global",
      },
    ]);
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

  test("returns discovered commands from the extension service", async () => {
    const xmpp = {
      async getDiscoItems(jid: string, node?: string) {
        if (jid === "example.com" && node === undefined) {
          return { items: [{ jid: "extensions.example.com", name: "Extensions" }] };
        }
        expect(jid).toBe("extensions.example.com");
        expect(node).toBe("http://jabber.org/protocol/commands");
        return {
          items: [
            { jid, node: "urn:waddle:extension:poll", name: "Poll" },
            { jid, node: "urn:waddle:extension:1:ai-chatbot", name: "Ask AI Chatbot" },
            { jid, node: "urn:waddle:extension:notes", name: "Notes" },
          ],
        };
      },
      async getDiscoInfo(jid: string) {
        expect(jid).toBe("extensions.example.com");
        return { features: ["urn:waddle:extension:1", "http://jabber.org/protocol/commands"] };
      },
    };

    expect(await discoverExtensionCommands(xmpp as any, "alice@example.com/web")).toEqual([
      { serviceJid: "extensions.example.com", node: "urn:waddle:extension:poll", name: "Poll", scope: "global" },
      { serviceJid: "extensions.example.com", node: "urn:waddle:extension:1:ai-chatbot", name: "Ask AI Chatbot", scope: "global" },
      { serviceJid: "extensions.example.com", node: "urn:waddle:extension:notes", name: "Notes", scope: "global" },
    ]);
  });

  test("discovers channel-scoped extension routes from route disco#info forms", async () => {
    const routeNode = "urn:waddle:link-board:1:channel:{room}:links";
    const xmpp = {
      async getDiscoItems(jid: string, node?: string) {
        if (jid === "example.com" && node === undefined) {
          return { items: [{ jid: "extensions.example.com", name: "Extensions" }] };
        }
        expect(jid).toBe("extensions.example.com");
        expect(node).toBeUndefined();
        return { items: [{ jid, node: routeNode, name: "Saved Links" }] };
      },
      async getDiscoInfo(jid: string, node?: string) {
        expect(jid).toBe("extensions.example.com");
        if (node === undefined) {
          return { features: ["urn:waddle:extension:1", "http://jabber.org/protocol/commands"] };
        }
        expect(node).toBe(routeNode);
        return {
          extensions: [{
            fields: [
              { name: "FORM_TYPE", value: "urn:waddle:extension:1:routes" },
              { name: "waddle#plugin_id", value: "link-board" },
              { name: "waddle#route_id", value: "saved-links" },
              { name: "waddle#route_label", value: "Saved Links" },
              { name: "waddle#route_scope", value: "channel" },
              { name: "waddle#route_surface", value: "gallery" },
              { name: "waddle#state_node", value: routeNode },
              { name: "waddle#payload_ns", value: "urn:waddle:link-board:1" },
            ],
          }],
        };
      },
    };

    expect(await discoverExtensionRoutes(xmpp as any, "alice@example.com/web")).toEqual([{
      serviceJid: "extensions.example.com",
      pluginId: "link-board",
      routeId: "saved-links",
      label: "Saved Links",
      scope: "channel",
      surface: "gallery",
      stateNode: routeNode,
      payloadNamespace: "urn:waddle:link-board:1",
    }]);
  });

  test("parses generic extension-item envelopes from PubSub state nodes", async () => {
    const route = {
      serviceJid: "extensions.example.com",
      pluginId: "link-board",
      routeId: "saved-links",
      label: "Saved Links",
      scope: "channel" as const,
      surface: "gallery" as const,
      stateNode: "urn:waddle:link-board:1:channel:{room}:links",
      payloadNamespace: "urn:waddle:link-board:1",
    };
    const xmpp = {
      async getItems(jid: string, node: string, opts?: { max?: number }) {
        expect(jid).toBe("extensions.example.com");
        expect(node).toBe("urn:waddle:link-board:1:channel:general@muc.example.com:links");
        expect(opts?.max).toBe(100);
        return {
          items: [{
            id: "https---example-org-post",
            content: {
              name: "extension-item",
              attributes: { xmlns: "urn:waddle:extension:1" },
              children: [
                {
                  name: "title",
                  attributes: { xmlns: "urn:waddle:extension:1" },
                  children: ["Example Post"],
                },
                {
                  name: "link",
                  attributes: { xmlns: "urn:waddle:extension:1", href: "https://example.org/post" },
                  children: [],
                },
                {
                  name: "description",
                  attributes: { xmlns: "urn:waddle:extension:1" },
                  children: ["A short summary of the post."],
                },
                {
                  name: "field",
                  attributes: { xmlns: "urn:waddle:extension:1", name: "saved-at", label: "Saved" },
                  children: ["2026-04-27T00:00:00Z"],
                },
              ],
            },
          }],
        };
      },
    };

    expect(resolveExtensionRouteStateNode(route, "general@muc.example.com"))
      .toBe("urn:waddle:link-board:1:channel:general@muc.example.com:links");
    expect(await fetchExtensionRouteItems(xmpp as any, route, "general@muc.example.com")).toEqual([{
      id: "https---example-org-post",
      title: "Example Post",
      link: { href: "https://example.org/post" },
      description: "A short summary of the post.",
      fields: [{ name: "saved-at", label: "Saved", value: "2026-04-27T00:00:00Z" }],
      options: [],
      actions: [],
    }]);
  });

  test("skips PubSub items that are not Waddle extension-item envelopes", async () => {
    const route = {
      serviceJid: "extensions.example.com",
      pluginId: "unknown-extension",
      routeId: "items",
      label: "Items",
      scope: "channel" as const,
      surface: "list" as const,
      stateNode: "urn:waddle:unknown:1:channel:{room}:items",
      payloadNamespace: "urn:waddle:unknown:1",
    };
    const xmpp = {
      async getItems() {
        return {
          items: [{
            id: "item-1",
            content: {
              getNamespace: () => "urn:waddle:unknown:1",
              getName: () => "saved-item",
              attributes: { url: "https://example.org/raw" },
              children: [{
                getNamespace: () => "urn:waddle:unknown:1",
                getName: () => "title",
                attributes: {},
                children: ["Raw item"],
              }],
            },
          }],
        };
      },
    };

    expect(await fetchExtensionRouteItems(xmpp as any, route, "general@muc.example.com")).toEqual([]);
  });

  test("parses extension-item envelopes with options and actions for poll-shaped routes", async () => {
    const route = {
      serviceJid: "extensions.example.com",
      pluginId: "decision-polls",
      routeId: "active-polls",
      label: "Polls",
      scope: "channel" as const,
      surface: "list" as const,
      stateNode: "urn:waddle:decision-polls:1:channel:{room}:polls",
      payloadNamespace: "urn:waddle:decision-polls:1",
    };
    const xmpp = {
      async getItems() {
        return {
          items: [{
            id: "poll-42",
            content: {
              name: "extension-item",
              attributes: { xmlns: "urn:waddle:extension:1" },
              children: [
                {
                  name: "title",
                  attributes: { xmlns: "urn:waddle:extension:1" },
                  children: ["Lunch tomorrow?"],
                },
                {
                  name: "subtitle",
                  attributes: { xmlns: "urn:waddle:extension:1" },
                  children: ["Open"],
                },
                {
                  name: "option",
                  attributes: { xmlns: "urn:waddle:extension:1", id: "a", label: "Pizza" },
                  children: [],
                },
                {
                  name: "option",
                  attributes: { xmlns: "urn:waddle:extension:1", id: "b", label: "Sushi" },
                  children: [],
                },
                {
                  name: "action",
                  attributes: { xmlns: "urn:waddle:extension:1", "launch-id": "vote-42", label: "Vote" },
                  children: [],
                },
              ],
            },
          }],
        };
      },
    };

    expect(await fetchExtensionRouteItems(xmpp as any, route, "general@muc.example.com")).toEqual([{
      id: "poll-42",
      title: "Lunch tomorrow?",
      subtitle: "Open",
      fields: [],
      options: [
        { id: "a", label: "Pizza" },
        { id: "b", label: "Sushi" },
      ],
      actions: [{ launchId: "vote-42", label: "Vote" }],
    }]);
  });

  test("treats successful empty PubSub route item responses as empty routes", async () => {
    const route = {
      serviceJid: "extensions.example.com",
      pluginId: "link-board",
      routeId: "saved-links",
      label: "Saved Links",
      scope: "channel" as const,
      surface: "gallery" as const,
      stateNode: "urn:waddle:link-board:1:channel:{room}:links",
      payloadNamespace: "urn:waddle:link-board:1",
    };
    const xmpp = {
      async getItems(jid: string, node: string, opts?: { max?: number }) {
        expect(jid).toBe("extensions.example.com");
        expect(node).toBe("urn:waddle:link-board:1:channel:general@muc.example.com:links");
        expect(opts?.max).toBe(100);
        return {};
      },
    };

    expect(await fetchExtensionRouteItems(xmpp as any, route, "general@muc.example.com")).toEqual([]);
  });

  test("normalizes item-not-found pubsub errors as unavailable routes", async () => {
    const route = {
      serviceJid: "extensions.example.com",
      pluginId: "link-board",
      routeId: "saved-links",
      label: "Saved Links",
      scope: "channel" as const,
      surface: "gallery" as const,
      stateNode: "urn:waddle:link-board:1:channel:{room}:links",
      payloadNamespace: "urn:waddle:link-board:1",
    };
    const xmpp = {
      async getItems() {
        throw { condition: "item-not-found", type: "cancel" };
      },
    };

    await expect(fetchExtensionRouteItems(xmpp as any, route, "general@muc.example.com"))
      .rejects.toThrow("Extension route was not found.");
  });

  test("normalizes nested item-not-found pubsub errors as unavailable routes", async () => {
    const route = {
      serviceJid: "extensions.example.com",
      pluginId: "decision-polls",
      routeId: "active-polls",
      label: "Polls",
      scope: "channel" as const,
      surface: "list" as const,
      stateNode: "urn:waddle:decision-polls:1:channel:{room}:polls",
      payloadNamespace: "urn:waddle:decision-polls:1",
    };
    const xmpp = {
      async getItems() {
        throw { error: { condition: "item-not-found", type: "cancel" } };
      },
    };

    await expect(fetchExtensionRouteItems(xmpp as any, route, "general@muc.example.com"))
      .rejects.toThrow("Extension route was not found.");
  });

  test("normalizes stanza.js item-not-found IQ errors as unavailable routes", async () => {
    const route = {
      serviceJid: "extensions.example.com",
      pluginId: "link-board",
      routeId: "saved-links",
      label: "Saved Links",
      scope: "channel" as const,
      surface: "gallery" as const,
      stateNode: "urn:waddle:link-board:1:channel:{room}:links",
      payloadNamespace: "urn:waddle:link-board:1",
    };
    const xmpp = {
      async getItems() {
        throw {
          id: "pubsub-1",
          type: "error",
          error: { type: "cancel", condition: "item-not-found" },
        };
      },
    };

    await expect(fetchExtensionRouteItems(xmpp as any, route, "general@muc.example.com"))
      .rejects.toThrow("Extension route was not found.");
  });

  test("normalizes pubsubError-only route load errors", async () => {
    const route = {
      serviceJid: "extensions.example.com",
      pluginId: "link-board",
      routeId: "saved-links",
      label: "Saved Links",
      scope: "channel" as const,
      surface: "gallery" as const,
      stateNode: "urn:waddle:link-board:1:channel:{room}:links",
      payloadNamespace: "urn:waddle:link-board:1",
    };
    const xmpp = {
      async getItems() {
        throw {
          id: "pubsub-1",
          type: "error",
          error: { type: "cancel", pubsubError: "item-not-found" },
        };
      },
    };

    await expect(fetchExtensionRouteItems(xmpp as any, route, "general@muc.example.com"))
      .rejects.toThrow("Extension route was not found.");
  });

  test("normalizes stanza.js route load errors into actionable Error messages", async () => {
    const route = {
      serviceJid: "extensions.example.com",
      pluginId: "link-board",
      routeId: "saved-links",
      label: "Saved Links",
      scope: "channel" as const,
      surface: "gallery" as const,
      stateNode: "urn:waddle:link-board:1:channel:{room}:links",
      payloadNamespace: "urn:waddle:link-board:1",
    };
    const xmpp = {
      async getItems() {
        throw {
          id: "pubsub-1",
          type: "error",
          error: { type: "auth", condition: "forbidden" },
        };
      },
    };

    await expect(fetchExtensionRouteItems(xmpp as any, route, "general@muc.example.com"))
      .rejects.toThrow("Extension route access was denied.");
  });

  test("normalizes remote server timeout route load errors", async () => {
    const route = {
      serviceJid: "extensions.example.com",
      pluginId: "link-board",
      routeId: "saved-links",
      label: "Saved Links",
      scope: "channel" as const,
      surface: "gallery" as const,
      stateNode: "urn:waddle:link-board:1:channel:{room}:links",
      payloadNamespace: "urn:waddle:link-board:1",
    };
    const xmpp = {
      async getItems() {
        throw { error: { condition: "remote-server-timeout", type: "wait" } };
      },
    };

    await expect(fetchExtensionRouteItems(xmpp as any, route, "general@muc.example.com"))
      .rejects.toThrow("Extension route request timed out.");
  });

  test("propagates non-item-not-found errors to the caller", async () => {
    const route = {
      serviceJid: "extensions.example.com",
      pluginId: "link-board",
      routeId: "saved-links",
      label: "Saved Links",
      scope: "channel" as const,
      surface: "gallery" as const,
      stateNode: "urn:waddle:link-board:1:channel:{room}:links",
      payloadNamespace: "urn:waddle:link-board:1",
    };
    const xmpp = {
      async getItems() {
        throw new Error("forbidden");
      },
    };

    await expect(fetchExtensionRouteItems(xmpp as any, route, "general@muc.example.com")).rejects.toThrow("forbidden");
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

  test("allows extension launches without source stanza metadata", () => {
    const iq = buildExtensionLaunchInvokeIq("alice@example.com/web", {
      ...launch,
      context: {
        waddleId: "waddle-123",
        roomJid: "pub@muc.example.com",
      },
    });

    expect(iq.command.form.fields.find((field) => field.name === "source-stanza-id")).toBeUndefined();
    expect(iq.command.form.fields.find((field) => field.name === "waddle#message_stanza_id")).toBeUndefined();
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
    expect(extensionCommandOutcome({
      status: "executing",
      form: { fields: [{ name: "payload#question", type: "text-single", value: "" }] },
      notes: [],
    })).toEqual({ state: "warning" });
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
      "pub@muc.example.com",
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
            { name: "waddle#room_jid", type: "hidden", value: "pub@muc.example.com" },
          ],
        },
      },
    });
  });
});
