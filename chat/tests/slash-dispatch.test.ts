import { describe, expect, test } from "bun:test";
import type { DiscoveredExtensionCommand } from "../src/lib/xmpp/extension-commands";
import { buildSlashInvocation } from "../src/lib/slash-dispatch";

const ai: DiscoveredExtensionCommand = {
  serviceJid: "extensions.example.com",
  node: "urn:waddle:extension:1:ai-chatbot",
  name: "Ask AI Chatbot",
  scope: "global",
  composerPrefix: "ai",
  inlineField: "prompt",
};

const poll: DiscoveredExtensionCommand = {
  serviceJid: "extensions.example.com",
  node: "urn:waddle:extension:1:decision-polls",
  name: "Create Decision Poll",
  scope: "channel",
  composerPrefix: "poll",
};

const stargate: DiscoveredExtensionCommand = {
  serviceJid: "extensions.example.com",
  node: "urn:waddle:extension:1:stargate-quotes",
  name: "/stargate",
  scope: "channel",
  composerPrefix: "stargate",
  composerExecute: true,
};

describe("buildSlashInvocation", () => {
  test("inline-submit when inlineField is declared and trailing has text", () => {
    expect(buildSlashInvocation(ai, "tell me a joke")).toEqual({
      kind: "inline-submit",
      command: ai,
      fieldName: "prompt",
      value: "tell me a joke",
    });
  });

  test("open-palette with no prefill when trailing is empty", () => {
    expect(buildSlashInvocation(ai, "")).toEqual({
      kind: "open-palette",
      command: ai,
    });
  });

  test("open-palette with first-required-field prefill when no inlineField but trailing exists", () => {
    expect(buildSlashInvocation(poll, "Best mascot?")).toEqual({
      kind: "open-palette",
      command: poll,
      prefillFirstRequired: "Best mascot?",
    });
  });

  test("open-palette without prefill when no inlineField and no trailing", () => {
    expect(buildSlashInvocation(poll, "")).toEqual({
      kind: "open-palette",
      command: poll,
    });
  });

  test("direct-execute for composer execute commands without trailing text", () => {
    expect(buildSlashInvocation(stargate, "")).toEqual({
      kind: "direct-execute",
      command: stargate,
    });
  });

  test("open-palette with prefill for composer execute commands with trailing text", () => {
    expect(buildSlashInvocation(stargate, "indeed")).toEqual({
      kind: "open-palette",
      command: stargate,
      prefillFirstRequired: "indeed",
    });
  });

  test("trims surrounding whitespace from the value", () => {
    expect(buildSlashInvocation(ai, "   hi there   ")).toEqual({
      kind: "inline-submit",
      command: ai,
      fieldName: "prompt",
      value: "hi there",
    });
  });

  test("treats whitespace-only trailing as empty", () => {
    expect(buildSlashInvocation(ai, "   ")).toEqual({
      kind: "open-palette",
      command: ai,
    });
  });
});
