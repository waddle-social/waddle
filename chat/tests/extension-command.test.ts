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
import { parseCommandIqResponse } from "../src/lib/xmpp/extension-commands/xml";

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
      async send_raw_iq(xml: string) {
        if (xml.includes('to="example.com"') && xml.includes("disco#items")) {
          return `<iq type="result"><query xmlns="http://jabber.org/protocol/disco#items"><item jid="extensions.example.com" name="Extensions" /></query></iq>`;
        }
        if (xml.includes('to="extensions.example.com"') && xml.includes("disco#info") && !xml.includes(" node=")) {
          return `<iq type="result"><query xmlns="http://jabber.org/protocol/disco#info"><feature var="urn:waddle:extension:1" /><feature var="http://jabber.org/protocol/commands" /></query></iq>`;
        }
        if (xml.includes('node="http://jabber.org/protocol/commands"')) {
          return `<iq type="result"><query xmlns="http://jabber.org/protocol/disco#items"><item jid="extensions.example.com" node="urn:waddle:extension:1:decision-polls" name="Create Decision Poll" /><item jid="extensions.example.com" node="urn:waddle:extension:1:ai-chatbot" name="Ask AI Chatbot" /></query></iq>`;
        }
        if (xml.includes('node="urn:waddle:extension:1:decision-polls"')) {
          return `<iq type="result"><query xmlns="http://jabber.org/protocol/disco#info"><x xmlns="jabber:x:data" type="result"><field var="FORM_TYPE"><value>urn:waddle:extension:1:command</value></field><field var="waddle#plugin_id"><value>decision-polls</value></field><field var="waddle#command_node"><value>urn:waddle:extension:1:decision-polls</value></field><field var="waddle#command_label"><value>Create Decision Poll</value></field><field var="waddle#command_scope"><value>channel</value></field></x></query></iq>`;
        }
        expect(xml).toContain('node="urn:waddle:extension:1:ai-chatbot"');
        return `<iq type="result"><query xmlns="http://jabber.org/protocol/disco#info"><x xmlns="jabber:x:data" type="result"><field var="FORM_TYPE"><value>urn:waddle:extension:1:command</value></field><field var="waddle#plugin_id"><value>ai-chatbot</value></field><field var="waddle#command_node"><value>urn:waddle:extension:1:ai-chatbot</value></field><field var="waddle#command_label"><value>Ask AI Chatbot</value></field><field var="waddle#command_scope"><value>global</value></field></x></query></iq>`;
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

  test("reads composer metadata from XEP-0128 disco metadata", async () => {
    const xmpp = {
      async send_raw_iq(xml: string) {
        if (xml.includes('to="example.com"') && xml.includes("disco#items")) {
          return `<iq type="result"><query xmlns="http://jabber.org/protocol/disco#items"><item jid="extensions.example.com" name="Extensions" /></query></iq>`;
        }
        if (xml.includes('to="extensions.example.com"') && xml.includes("disco#info") && !xml.includes(" node=")) {
          return `<iq type="result"><query xmlns="http://jabber.org/protocol/disco#info"><feature var="urn:waddle:extension:1" /><feature var="http://jabber.org/protocol/commands" /></query></iq>`;
        }
        if (xml.includes('node="http://jabber.org/protocol/commands"')) {
          return `<iq type="result"><query xmlns="http://jabber.org/protocol/disco#items"><item jid="extensions.example.com" node="urn:waddle:extension:1:ai-chatbot" name="Ask AI Chatbot" /><item jid="extensions.example.com" node="urn:waddle:extension:1:decision-polls" name="Create Decision Poll" /><item jid="extensions.example.com" node="urn:waddle:extension:1:stargate-quotes" name="/stargate" /></query></iq>`;
        }
        if (xml.includes('node="urn:waddle:extension:1:ai-chatbot"')) {
          return `<iq type="result"><query xmlns="http://jabber.org/protocol/disco#info"><x xmlns="jabber:x:data" type="result"><field var="FORM_TYPE"><value>urn:waddle:extension:1:command</value></field><field var="waddle#command_scope"><value>global</value></field><field var="waddle#composer_prefix"><value>ai</value></field><field var="waddle#inline_field"><value>prompt</value></field></x></query></iq>`;
        }
        if (xml.includes('node="urn:waddle:extension:1:decision-polls"')) {
          return `<iq type="result"><query xmlns="http://jabber.org/protocol/disco#info"><x xmlns="jabber:x:data" type="result"><field var="FORM_TYPE"><value>urn:waddle:extension:1:command</value></field><field var="waddle#command_scope"><value>channel</value></field><field var="waddle#composer_prefix"><value>poll</value></field></x></query></iq>`;
        }
        expect(xml).toContain('node="urn:waddle:extension:1:stargate-quotes"');
        return `<iq type="result"><query xmlns="http://jabber.org/protocol/disco#info"><x xmlns="jabber:x:data" type="result"><field var="FORM_TYPE"><value>urn:waddle:extension:1:command</value></field><field var="waddle#command_scope"><value>channel</value></field><field var="waddle#composer_prefix"><value>stargate</value></field><field var="waddle#composer_execute"><value>true</value></field></x></query></iq>`;
      },
    };

    const commands = await discoverExtensionCommands(xmpp as any, "alice@example.com/web");
    expect(commands).toEqual([
      {
        serviceJid: "extensions.example.com",
        node: "urn:waddle:extension:1:ai-chatbot",
        name: "Ask AI Chatbot",
        scope: "global",
        composerPrefix: "ai",
        inlineField: "prompt",
      },
      {
        serviceJid: "extensions.example.com",
        node: "urn:waddle:extension:1:decision-polls",
        name: "Create Decision Poll",
        scope: "channel",
        composerPrefix: "poll",
      },
      {
        serviceJid: "extensions.example.com",
        node: "urn:waddle:extension:1:stargate-quotes",
        name: "/stargate",
        scope: "channel",
        composerPrefix: "stargate",
        composerExecute: true,
      },
    ]);
  });

  test("falls back to raw disco XML when typed command info omits metadata forms", async () => {
    const xmpp = {
      async getDiscoItems(jid: string, node?: string) {
        if (jid === "example.com" && !node) return { items: [{ jid: "extensions.example.com" }] };
        if (jid === "extensions.example.com" && node === "http://jabber.org/protocol/commands") {
          return {
            items: [{
              jid: "extensions.example.com",
              node: "urn:waddle:extension:1:ai-chatbot",
              name: "Ask AI Chatbot",
            }],
          };
        }
        return { items: [] };
      },
      async getDiscoInfo(_jid: string, node?: string) {
        if (!node) {
          return {
            features: ["urn:waddle:extension:1", "http://jabber.org/protocol/commands"],
            extensions: [],
          };
        }
        return { features: ["http://jabber.org/protocol/commands"], extensions: [] };
      },
      async send_raw_iq(xml: string) {
        expect(xml).toContain('node="urn:waddle:extension:1:ai-chatbot"');
        return `<iq type="result"><query xmlns="http://jabber.org/protocol/disco#info"><x xmlns="jabber:x:data" type="result"><field var="FORM_TYPE"><value>urn:waddle:extension:1:command</value></field><field var="waddle#command_scope"><value>global</value></field><field var="waddle#composer_prefix"><value>ai</value></field><field var="waddle#inline_field"><value>prompt</value></field></x></query></iq>`;
      },
    };

    expect(await discoverExtensionCommands(xmpp as any, "alice@example.com/web")).toEqual([{
      serviceJid: "extensions.example.com",
      node: "urn:waddle:extension:1:ai-chatbot",
      name: "Ask AI Chatbot",
      scope: "global",
      composerPrefix: "ai",
      inlineField: "prompt",
    }]);
  });

  test("treats an empty composer_prefix value as absent rather than a zero-length match", async () => {
    const xmpp = {
      async send_raw_iq(xml: string) {
        if (xml.includes('to="example.com"') && xml.includes("disco#items")) {
          return `<iq type="result"><query xmlns="http://jabber.org/protocol/disco#items"><item jid="extensions.example.com" name="Extensions" /></query></iq>`;
        }
        if (xml.includes('to="extensions.example.com"') && xml.includes("disco#info") && !xml.includes(" node=")) {
          return `<iq type="result"><query xmlns="http://jabber.org/protocol/disco#info"><feature var="urn:waddle:extension:1" /><feature var="http://jabber.org/protocol/commands" /></query></iq>`;
        }
        if (xml.includes('node="http://jabber.org/protocol/commands"')) {
          return `<iq type="result"><query xmlns="http://jabber.org/protocol/disco#items"><item jid="extensions.example.com" node="urn:waddle:extension:1:noprefix" name="No Prefix" /></query></iq>`;
        }
        expect(xml).toContain('node="urn:waddle:extension:1:noprefix"');
        return `<iq type="result"><query xmlns="http://jabber.org/protocol/disco#info"><x xmlns="jabber:x:data" type="result"><field var="FORM_TYPE"><value>urn:waddle:extension:1:command</value></field><field var="waddle#command_scope"><value>global</value></field><field var="waddle#composer_prefix"><value></value></field><field var="waddle#inline_field" /></x></query></iq>`;
      },
    };

    const commands = await discoverExtensionCommands(xmpp as any, "alice@example.com/web");
    expect(commands).toEqual([
      {
        serviceJid: "extensions.example.com",
        node: "urn:waddle:extension:1:noprefix",
        name: "No Prefix",
        scope: "global",
      },
    ]);
  });

  test("prefers the advertised Waddle extension service over generic command services", async () => {
    const commandItemTargets: string[] = [];
    const xmpp = {
      async send_raw_iq(xml: string) {
        const target = xml.match(/\bto="([^"]+)"/)?.[1] ?? "";
        if (xml.includes("disco#items") && !xml.includes('node="http://jabber.org/protocol/commands"')) {
          expect(target).toBe("example.com");
          return `<iq type="result"><query xmlns="http://jabber.org/protocol/disco#items"><item jid="automation.example.com" name="Automation" /><item jid="extensions.example.com" name="Extensions" /></query></iq>`;
        }
        if (xml.includes("disco#info") && !xml.includes(" node=")) {
          if (target === "automation.example.com") {
            return `<iq type="result"><query xmlns="http://jabber.org/protocol/disco#info"><feature var="http://jabber.org/protocol/commands" /></query></iq>`;
          }
          expect(target).toBe("extensions.example.com");
          return `<iq type="result"><query xmlns="http://jabber.org/protocol/disco#info"><feature var="urn:waddle:extension:1" /><feature var="http://jabber.org/protocol/commands" /></query></iq>`;
        }
        if (xml.includes("disco#items") && xml.includes('node="http://jabber.org/protocol/commands"')) {
          commandItemTargets.push(target);
          return `<iq type="result"><query xmlns="http://jabber.org/protocol/disco#items"><item jid="extensions.example.com" node="urn:waddle:extension:1:decision-polls" name="Create Decision Poll" /></query></iq>`;
        }
        expect(xml).toContain('node="urn:waddle:extension:1:decision-polls"');
        return `<iq type="result"><query xmlns="http://jabber.org/protocol/disco#info"><x xmlns="jabber:x:data" type="result"><field var="FORM_TYPE"><value>urn:waddle:extension:1:command</value></field><field var="waddle#plugin_id"><value>decision-polls</value></field><field var="waddle#command_node"><value>urn:waddle:extension:1:decision-polls</value></field><field var="waddle#command_label"><value>Create Decision Poll</value></field><field var="waddle#command_scope"><value>channel</value></field></x></query></iq>`;
      },
    };

    expect(await discoverExtensionCommands(xmpp as any, "alice@example.com/web")).toEqual([{
      serviceJid: "extensions.example.com",
      node: "urn:waddle:extension:1:decision-polls",
      name: "Create Decision Poll",
      scope: "channel",
    }]);
    expect(commandItemTargets).toEqual(["extensions.example.com"]);
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

  test("uses the launch-advertised command node for action invocation", () => {
    const iq = buildExtensionLaunchInvokeIq("alice@example.com/web", {
      ...launch,
      commandNode: "urn:waddle:extension:1:decision-polls",
    });

    expect(iq.command.node).toBe("urn:waddle:extension:1:decision-polls");
  });

  test("builds plain XEP-0050 execute requests for discovered commands", async () => {
    let sent = "";
    const xmpp = {
      async send_raw_iq(xml: string) {
        sent = xml;
        return `<iq type="result"><command xmlns="http://jabber.org/protocol/commands" status="completed" /></iq>`;
      },
    };

    await invokeExtensionCommand(
      xmpp as any,
      "alice@example.com/web",
      { serviceJid: "extensions.example.com", node: "urn:waddle:extension:noop", name: "Noop" },
    );

    expect(sent).toContain('type="set"');
    expect(sent).toContain('to="extensions.example.com"');
    expect(sent).toContain('node="urn:waddle:extension:noop"');
    expect(sent).toContain('action="execute"');
    expect(sent).not.toContain('jabber:x:data');
  });

  test("adds active room context to initial XEP-0050 execute requests", async () => {
    let sent = "";
    const xmpp = {
      async send_raw_iq(xml: string) {
        sent = xml;
        return `<iq type="result"><command xmlns="http://jabber.org/protocol/commands" status="completed" /></iq>`;
      },
    };

    await invokeExtensionCommand(
      xmpp as any,
      "alice@example.com/web",
      { serviceJid: "extensions.example.com", node: "urn:waddle:extension:1:stargate-quotes", name: "/stargate" },
      "pub@muc.example.com",
    );

    expect(sent).toContain('type="set"');
    expect(sent).toContain('node="urn:waddle:extension:1:stargate-quotes"');
    expect(sent).toContain('action="execute"');
    expect(sent).toContain('<x xmlns="jabber:x:data" type="submit">');
    expect(sent).toContain('<field var="waddle#room_jid"><value>pub@muc.example.com</value></field>');
  });

  test("returns discovered commands from the extension service", async () => {
    const xmpp = {
      async send_raw_iq(xml: string) {
        if (xml.includes('disco#info')) {
          return `<iq type="result"><query xmlns="http://jabber.org/protocol/disco#info"><feature var="http://jabber.org/protocol/commands" /></query></iq>`;
        }
        return `<iq type="result"><query xmlns="http://jabber.org/protocol/disco#items"><item jid="extensions.example.com" node="urn:waddle:extension:poll" name="Poll" /><item jid="extensions.example.com" node="urn:waddle:extension:1:ai-chatbot" name="Ask AI Chatbot" /><item jid="extensions.example.com" node="urn:waddle:extension:notes" name="Notes" /></query></iq>`;
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
      async discover_extension_routes() {
        return [{
          service_jid: "extensions.example.com",
          plugin_id: "link-board",
          route_id: "saved-links",
          label: "Saved Links",
          scope: "channel",
          surface: "gallery",
          state_node: routeNode,
          payload_namespace: "urn:waddle:link-board:1",
        }];
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

  test("normalizes typed extension route items from the Rust client", async () => {
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
      async fetch_extension_route_items(receivedRoute: unknown, roomJid: string) {
        expect(receivedRoute).toEqual(route);
        expect(roomJid).toBe("general@muc.example.com");
        return [{
          id: "https---example-org-post",
          title: "Example Post",
          link: { href: "https://example.org/post" },
          description: "A short summary of the post.",
          fields: [{ name: "saved-at", label: "Saved", value: "2026-04-27T00:00:00Z" }],
          options: [],
          actions: [],
        }];
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

  test("normalizes typed route items with options and actions for poll-shaped routes", async () => {
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
      async fetch_extension_route_items() {
        return [{
          id: "poll-42",
          title: "Lunch tomorrow?",
          subtitle: "Open",
          options: [
            { id: "a", label: "Pizza" },
            { id: "b", label: "Sushi" },
          ],
          actions: [{
            launch: {
              id: "vote-42",
              plugin_id: "decision-polls",
              action_id: "vote",
              command_node: "urn:waddle:extensions:invoke",
              label: "Vote",
              launch_token: "signed-token",
              expires_at: "2026-04-27T00:00:00Z",
              waddle_id: "alice@example.com",
              room_jid: "general@muc.example.com",
            },
          }],
          fields: [],
        }];
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
      actions: [{
        launchId: "vote-42",
        label: "Vote",
        launch: {
          id: "vote-42",
          pluginId: "decision-polls",
          actionId: "vote",
          commandNode: "urn:waddle:extensions:invoke",
          launchToken: "signed-token",
          expiresAt: "2026-04-27T00:00:00Z",
          label: "Vote",
          context: {
            waddleId: "alice@example.com",
            roomJid: "general@muc.example.com",
          },
          payloads: [],
        },
      }],
    }]);
  });

  test("drops route item actions without launch metadata", async () => {
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
      async fetch_extension_route_items() {
        return [{
          id: "poll-42",
          title: "Lunch tomorrow?",
          options: [],
          actions: [{ launch_id: "vote-42", label: "Vote" }],
          fields: [],
        }];
      },
    };

    expect(await fetchExtensionRouteItems(xmpp as any, route, "general@muc.example.com")).toEqual([{
      id: "poll-42",
      title: "Lunch tomorrow?",
      fields: [],
      options: [],
      actions: [],
    }]);
  });

  test("treats successful empty Rust route item responses as empty routes", async () => {
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
      async fetch_extension_route_items() {
        return [];
      },
    };

    expect(await fetchExtensionRouteItems(xmpp as any, route, "general@muc.example.com")).toEqual([]);
  });

  test("propagates typed Rust route load errors to the caller", async () => {
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
      async fetch_extension_route_items() {
        throw new Error("Extension route access was denied.");
      },
    };

    await expect(fetchExtensionRouteItems(xmpp as any, route, "general@muc.example.com"))
      .rejects.toThrow("Extension route access was denied.");
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

  test("wire responses without <actions/> imply complete plus cancel while executing", () => {
    const result = parseCommandIqResponse(
      '<iq type="result"><command xmlns="http://jabber.org/protocol/commands" node="urn:waddle:extension:1:ai-chatbot" sessionid="s-1" status="executing">' +
        '<x xmlns="jabber:x:data" type="form"><field var="prompt" type="text-single"><value/></field></x></command></iq>',
    );

    expect(result.status).toBe("executing");
    expect(result.actions).toEqual({ allowed: ["complete", "cancel"] });
  });

  test("a self-closing actions element suppresses the implied complete", () => {
    const result = parseCommandIqResponse(
      '<iq type="result"><command xmlns="http://jabber.org/protocol/commands" node="urn:waddle:extension:1:ai-chatbot" sessionid="s-1" status="executing">' +
        '<actions/><x xmlns="jabber:x:data" type="form"><field var="prompt" type="text-single"><value/></field></x></command></iq>',
    );

    // <actions/> is the server saying "no forward actions": cancel only,
    // matching the Rust parser (get_child sees the element as present).
    expect(result.actions).toEqual({ allowed: ["cancel"] });
  });

  test("wire responses with completed status carry no implied actions", () => {
    const result = parseCommandIqResponse(
      '<iq type="result"><command xmlns="http://jabber.org/protocol/commands" node="urn:waddle:extension:1:ai-chatbot" status="completed"/></iq>',
    );

    expect(result.status).toBe("completed");
    expect(result.actions).toBeUndefined();
  });

  test("preserves XEP-0004 multi-value fields from command form values", () => {
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

  test("submits XEP-0050 session id and XEP-0004 multi values using protocol field names", async () => {
    let sent = "";
    const xmpp = {
      async send_raw_iq(xml: string) {
        sent = xml;
        return `<iq type="result"><command xmlns="http://jabber.org/protocol/commands" status="completed" sessionid="session-1" /></iq>`;
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

    expect(sent).toContain('to="extensions.example.com"');
    expect(sent).toContain('node="urn:waddle:extension:poll"');
    expect(sent).toContain('action="next"');
    expect(sent).toContain('sessionid="session-1"');
    expect(sent).toContain('<field var="payload#choices"><value>yes</value><value>maybe</value></field>');
    expect(sent).toContain('<field var="payload#notify" type="boolean"><value>false</value></field>');
    expect(sent).toContain('<field var="hidden#multi"><value>alpha</value><value>beta</value></field>');
    expect(sent).toContain('<field var="waddle#room_jid"><value>pub@muc.example.com</value></field>');
    expect(sent).not.toContain('fixed:Do not send me');
  });
});
