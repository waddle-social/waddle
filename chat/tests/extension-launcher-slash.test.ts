import { describe, expect, test } from "bun:test";
import { ref, type Ref } from "vue";
import { useExtensionLauncher } from "../src/channels/extension-launcher";
import type {
  DiscoveredExtensionCommand,
  ExtensionCommandAction,
  ExtensionCommandFormField,
  ExtensionCommandResult,
} from "../src/lib/xmpp/extension-commands";
import type { SlashInvocation } from "../src/lib/slash-dispatch";

const aiCommand: DiscoveredExtensionCommand = {
  serviceJid: "extensions.example.com",
  node: "urn:waddle:extension:1:ai-chatbot",
  name: "Ask AI Chatbot",
  scope: "global",
  composerPrefix: "ai",
  inlineField: "prompt",
};

const pollCommand: DiscoveredExtensionCommand = {
  serviceJid: "extensions.example.com",
  node: "urn:waddle:extension:1:decision-polls",
  name: "Create Decision Poll",
  scope: "channel",
  composerPrefix: "poll",
};

const genericCommand: DiscoveredExtensionCommand = {
  serviceJid: "extensions.example.com",
  node: "urn:waddle:extension:1:generic",
  name: "Generic Extension",
  scope: "channel",
  composerPrefix: "generic",
  inlineField: "prompt",
};

interface SubmitCall {
  command: DiscoveredExtensionCommand;
  sessionId: string;
  fields: ExtensionCommandFormField[];
  action?: ExtensionCommandAction;
  roomJid?: string;
}

function field(name: string, overrides: Partial<ExtensionCommandFormField> = {}): ExtensionCommandFormField {
  return {
    name,
    label: name,
    type: "text-single",
    value: "",
    values: [],
    options: [],
    required: false,
    blocked: false,
    hidden: false,
    ...overrides,
  };
}

function executingResult(fields: ExtensionCommandFormField[], allowed: ExtensionCommandAction[] = ["complete", "cancel"]): ExtensionCommandResult {
  return {
    status: "executing",
    sessionId: "session-1",
    actions: { allowed },
    notes: [],
    form: {
      fields: fields.map((f) => ({
        var: f.name,
        type: f.type,
        label: f.label,
        required: f.required,
        values: f.values,
        rawValues: f.values,
        options: f.options,
      })),
    },
  };
}

function fakeClient(executeResults: ExtensionCommandResult[], options: {
  events?: string[] | undefined;
  submitError?: Error | undefined;
} = {}): {
  client: Ref<unknown>;
  submitCalls: SubmitCall[];
  invokeCalls: DiscoveredExtensionCommand[];
} {
  const submitCalls: SubmitCall[] = [];
  const invokeCalls: DiscoveredExtensionCommand[] = [];
  const queue = [...executeResults];
  const fake = {
    async invokeExtensionCommand(command: DiscoveredExtensionCommand): Promise<ExtensionCommandResult> {
      invokeCalls.push(command);
      return queue.shift() ?? { status: "completed", notes: [] };
    },
    async submitExtensionCommandForm(
      command: DiscoveredExtensionCommand,
      sessionId: string,
      fields: ExtensionCommandFormField[],
      action?: ExtensionCommandAction,
      roomJid?: string,
    ): Promise<ExtensionCommandResult> {
      options.events?.push("submit");
      submitCalls.push({ command, sessionId, fields, action, roomJid });
      if (options.submitError) throw options.submitError;
      return { status: "completed", notes: [] };
    },
    async discoverExtensionCommands(): Promise<DiscoveredExtensionCommand[]> {
      return [];
    },
  };
  return { client: ref(fake), submitCalls, invokeCalls };
}

function createLauncher(
  executeResults: ExtensionCommandResult[],
  options: {
    roomJid?: string | null;
    sendPublicChannelMessage?: (body: string) => Promise<void>;
    events?: string[] | undefined;
    submitError?: Error | undefined;
  } = {},
) {
  const fake = fakeClient(executeResults, {
    events: options.events,
    submitError: options.submitError,
  });
  const launcher = useExtensionLauncher({
    xmppClient: fake.client as never,
    roomJid: ref(options.roomJid ?? null),
    invokeExtensionAction: ref(undefined),
    sendPublicChannelMessage: ref(options.sendPublicChannelMessage),
    focusPalette: () => {},
    focusComposerExtensions: () => {},
  });
  return { ...fake, launcher };
}

describe("dispatchSlashInvocation: inline-submit", () => {
  test("auto-submits with the inline field set when the form advertises `complete`", async () => {
    const { launcher, submitCalls, invokeCalls } = createLauncher([
      executingResult([field("prompt", { required: true })], ["complete", "cancel"]),
    ]);

    const invocation: SlashInvocation = {
      kind: "inline-submit",
      command: aiCommand,
      fieldName: "prompt",
      value: "tell me a joke",
    };
    const ok = await launcher.dispatchSlashInvocation(invocation);

    expect(ok).toBe(true);
    expect(invokeCalls).toEqual([aiCommand]);
    expect(submitCalls).toHaveLength(1);
    expect(submitCalls[0].action).toBe("complete");
    expect(submitCalls[0].fields.find((f) => f.name === "prompt")?.values).toEqual([
      "tell me a joke",
    ]);
    expect(launcher.open.value).toBe(false);
  });

  test("passes the active room into XEP-0050 inline command submissions", async () => {
    const { launcher, submitCalls } = createLauncher([
      executingResult([field("prompt", { required: true })], ["complete", "cancel"]),
    ], { roomJid: "pub@muc.example.com" });

    await launcher.dispatchSlashInvocation({
      kind: "inline-submit",
      command: aiCommand,
      fieldName: "prompt",
      value: "tell the room",
    });

    expect(submitCalls).toHaveLength(1);
    expect(submitCalls[0].roomJid).toBe("pub@muc.example.com");
  });

  test("makes inline /ai public in channel context", async () => {
    const events: string[] = [];
    const { launcher, submitCalls } = createLauncher([
      executingResult([
        field("prompt", { required: true }),
        field("output", {
          type: "list-single",
          value: "private",
          values: ["private"],
          options: [
            { label: "Private", value: "private" },
            { label: "Post to channel", value: "channel" },
          ],
        }),
      ], ["complete", "cancel"]),
    ], {
      roomJid: "pub@muc.example.com",
      events,
      sendPublicChannelMessage: async (body) => {
        events.push(`public:${body}`);
      },
    });

    const ok = await launcher.dispatchSlashInvocation({
      kind: "inline-submit",
      command: aiCommand,
      fieldName: "prompt",
      value: "tell the room",
    });

    expect(ok).toBe(true);
    expect(events).toEqual(["public:tell the room", "submit"]);
    expect(submitCalls).toHaveLength(1);
    expect(submitCalls[0].fields.find((f) => f.name === "output")?.values).toEqual([
      "channel",
    ]);
  });

  test("does not submit inline /ai when the public prompt fails to send", async () => {
    const events: string[] = [];
    const { launcher, submitCalls } = createLauncher([
      executingResult([
        field("prompt", { required: true }),
        field("output", {
          type: "list-single",
          value: "private",
          values: ["private"],
          options: [
            { label: "Private", value: "private" },
            { label: "Post to channel", value: "channel" },
          ],
        }),
      ], ["complete", "cancel"]),
    ], {
      roomJid: "pub@muc.example.com",
      events,
      sendPublicChannelMessage: async () => {
        events.push("public-failed");
        throw new Error("channel send failed");
      },
    });

    const ok = await launcher.dispatchSlashInvocation({
      kind: "inline-submit",
      command: aiCommand,
      fieldName: "prompt",
      value: "tell the room",
    });

    expect(ok).toBe(false);
    expect(events).toEqual(["public-failed"]);
    expect(submitCalls).toEqual([]);
    expect(launcher.commandStates.value[aiCommand.node]?.state).toBe("error");
    expect(launcher.commandStates.value[aiCommand.node]?.detail).toBe("channel send failed");
  });

  test("returns false and surfaces an error state when the XMPP client is null", async () => {
    const launcher = useExtensionLauncher({
      xmppClient: ref(null) as never,
      roomJid: ref(null),
      invokeExtensionAction: ref(undefined),
      focusPalette: () => {},
      focusComposerExtensions: () => {},
    });

    const ok = await launcher.dispatchSlashInvocation({
      kind: "inline-submit",
      command: aiCommand,
      fieldName: "prompt",
      value: "tell me a joke",
    });

    expect(ok).toBe(false);
    expect(launcher.state.value).toBe("error");
    expect(launcher.detail.value).toContain("disconnected");
  });

  test("falls back to the palette when the executing form does not allow `complete`", async () => {
    const { launcher, submitCalls } = createLauncher([
      executingResult([field("prompt", { required: true })], ["next", "cancel"]),
    ]);

    await launcher.dispatchSlashInvocation({
      kind: "inline-submit",
      command: aiCommand,
      fieldName: "prompt",
      value: "tell me a joke",
    });

    expect(submitCalls).toEqual([]);
    expect(launcher.open.value).toBe(true);
    const stored = launcher.commandForms.value[aiCommand.node]?.fields.find((f) => f.name === "prompt");
    expect(stored?.values).toEqual(["tell me a joke"]);
  });

  test("does not submit when the server returns no executing form", async () => {
    const { launcher, submitCalls } = createLauncher([
      { status: "completed", notes: [] },
    ]);

    await launcher.dispatchSlashInvocation({
      kind: "inline-submit",
      command: aiCommand,
      fieldName: "prompt",
      value: "stale prompt",
    });

    expect(submitCalls).toEqual([]);
    expect(launcher.commandStates.value[aiCommand.node]?.state).toBe("success");
  });

  test("returns false when inline command submission fails", async () => {
    const { launcher, submitCalls } = createLauncher([
      executingResult([field("prompt", { required: true })], ["complete", "cancel"]),
    ], {
      submitError: new Error("submit failed"),
    });

    const ok = await launcher.dispatchSlashInvocation({
      kind: "inline-submit",
      command: aiCommand,
      fieldName: "prompt",
      value: "tell me a joke",
    });

    expect(ok).toBe(false);
    expect(submitCalls).toHaveLength(1);
    expect(launcher.commandStates.value[aiCommand.node]?.state).toBe("error");
    expect(launcher.commandStates.value[aiCommand.node]?.detail).toBe("submit failed");
  });

  test("returns false after posting public /ai prompt when command submission fails", async () => {
    const events: string[] = [];
    const { launcher, submitCalls } = createLauncher([
      executingResult([
        field("prompt", { required: true }),
        field("output", {
          type: "list-single",
          value: "private",
          values: ["private"],
          options: [
            { label: "Private", value: "private" },
            { label: "Post to channel", value: "channel" },
          ],
        }),
      ], ["complete", "cancel"]),
    ], {
      roomJid: "pub@muc.example.com",
      events,
      submitError: new Error("submit failed"),
      sendPublicChannelMessage: async (body) => {
        events.push(`public:${body}`);
      },
    });

    const ok = await launcher.dispatchSlashInvocation({
      kind: "inline-submit",
      command: aiCommand,
      fieldName: "prompt",
      value: "tell the room",
    });

    expect(ok).toBe(false);
    expect(events).toEqual(["public:tell the room", "submit"]);
    expect(submitCalls).toHaveLength(1);
    expect(launcher.commandStates.value[aiCommand.node]?.state).toBe("error");
    expect(launcher.commandStates.value[aiCommand.node]?.detail).toBe("submit failed");
  });
});

describe("dispatchSlashInvocation: open-palette", () => {
  test("opens the palette and prefills the first required visible field", async () => {
    const { launcher, submitCalls } = createLauncher([
      executingResult([
        field("question", { required: true }),
        field("options", { required: true }),
      ]),
    ]);

    await launcher.dispatchSlashInvocation({
      kind: "open-palette",
      command: pollCommand,
      prefillFirstRequired: "Best mascot?",
    });

    expect(submitCalls).toEqual([]);
    expect(launcher.open.value).toBe(true);
    const stored = launcher.commandForms.value[pollCommand.node]?.fields.find((f) => f.name === "question");
    expect(stored?.values).toEqual(["Best mascot?"]);
  });

  test("skips prefill when the form has no required visible field", async () => {
    const { launcher } = createLauncher([
      executingResult([field("note", { required: false })]),
    ]);

    await launcher.dispatchSlashInvocation({
      kind: "open-palette",
      command: pollCommand,
      prefillFirstRequired: "ignored",
    });

    const stored = launcher.commandForms.value[pollCommand.node]?.fields.find((f) => f.name === "note");
    expect(stored?.values).toEqual([]);
  });

  test("opens the palette empty when there is no prefill", async () => {
    const { launcher } = createLauncher([
      executingResult([field("question", { required: true })]),
    ]);

    await launcher.dispatchSlashInvocation({
      kind: "open-palette",
      command: pollCommand,
    });

    expect(launcher.open.value).toBe(true);
    const stored = launcher.commandForms.value[pollCommand.node]?.fields.find((f) => f.name === "question");
    expect(stored?.values).toEqual([]);
  });
});

describe("submitForm: public channel output", () => {
  test("posts the user's prompt to the room before submitting the command", async () => {
    const events: string[] = [];
    const { launcher, submitCalls } = createLauncher([
      executingResult([
        field("prompt", { required: true }),
        field("output", { required: true, value: "private", values: ["private"] }),
      ]),
    ], {
      roomJid: "pub@muc.example.com",
      events,
      sendPublicChannelMessage: async (body) => {
        events.push(`public:${body}`);
      },
    });
    await launcher.invokeCommand(aiCommand);
    launcher.updateField(aiCommand.node, "prompt", ["what changed?"]);
    launcher.updateField(aiCommand.node, "output", ["channel"]);

    const ok = await launcher.submitForm(aiCommand, "complete");

    expect(ok).toBe(true);
    expect(events).toEqual(["public:what changed?", "submit"]);
    expect(submitCalls).toHaveLength(1);
    expect(submitCalls[0].roomJid).toBe("pub@muc.example.com");
  });

  test("does not submit the command when posting the public prompt fails", async () => {
    const events: string[] = [];
    const { launcher, submitCalls } = createLauncher([
      executingResult([
        field("prompt", { required: true }),
        field("output", { required: true, value: "private", values: ["private"] }),
      ]),
    ], {
      roomJid: "pub@muc.example.com",
      events,
      sendPublicChannelMessage: async () => {
        events.push("public-failed");
        throw new Error("could not send prompt");
      },
    });
    await launcher.invokeCommand(aiCommand);
    launcher.updateField(aiCommand.node, "prompt", ["what changed?"]);
    launcher.updateField(aiCommand.node, "output", ["channel"]);

    const ok = await launcher.submitForm(aiCommand, "complete");

    expect(ok).toBe(false);
    expect(events).toEqual(["public-failed"]);
    expect(submitCalls).toEqual([]);
    expect(launcher.commandStates.value[aiCommand.node]?.state).toBe("error");
    expect(launcher.commandStates.value[aiCommand.node]?.detail).toBe("could not send prompt");
  });

  test("does not echo non-AI extension prompts with output=channel", async () => {
    const events: string[] = [];
    const { launcher, submitCalls } = createLauncher([
      executingResult([
        field("prompt", { required: true }),
        field("output", { required: true, value: "channel", values: ["channel"] }),
      ]),
    ], {
      roomJid: "pub@muc.example.com",
      events,
      sendPublicChannelMessage: async (body) => {
        events.push(`public:${body}`);
      },
    });
    await launcher.invokeCommand(genericCommand);
    launcher.updateField(genericCommand.node, "prompt", ["generic public text"]);

    const ok = await launcher.submitForm(genericCommand, "complete");

    expect(ok).toBe(true);
    expect(events).toEqual(["submit"]);
    expect(submitCalls).toHaveLength(1);
    expect(submitCalls[0].command).toBe(genericCommand);
  });
});
