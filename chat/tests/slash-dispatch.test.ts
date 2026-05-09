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
