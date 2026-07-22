import { describe, expect, test } from "bun:test";
import { ref, type Ref } from "vue";
import { useExtensionLauncher } from "../src/channels/extension-launcher";
import type { ExtensionAnnotationAction } from "../src/lib/chat-ui";
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

const stargateCommand: DiscoveredExtensionCommand = {
  serviceJid: "extensions.example.com",
  node: "urn:waddle:extension:1:stargate-quotes",
  name: "/stargate",
  scope: "channel",
  composerPrefix: "stargate",
  composerExecute: true,
};

interface SubmitCall {
  command: DiscoveredExtensionCommand;
  sessionId: string;
  fields: ExtensionCommandFormField[];
  action?: ExtensionCommandAction;
  roomJid?: string;
}

interface InvokeCall {
  command: DiscoveredExtensionCommand;
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

function launchResult(): ExtensionCommandResult {
  return {
    status: "completed",
    notes: [],
    form: {
      fields: [
        { name: "launch-count", value: "1" },
        { name: "launch#0#id", value: "route-1" },
        { name: "launch#0#plugin", value: "generic" },
        { name: "launch#0#action", value: "finish" },
        { name: "launch#0#command-node", value: genericCommand.node },
        { name: "launch#0#label", value: "Finish" },
        { name: "launch#0#waddle-id", value: "waddle-1" },
        { name: "launch#0#token", value: "token-1" },
        { name: "launch#0#expires-at", value: "2026-06-18T12:00:00Z" },
      ],
    },
  };
}

function fakeClient(executeResults: ExtensionCommandResult[], options: {
  events?: string[] | undefined;
  invokeError?: Error | undefined;
  submitError?: Error | undefined;
  submitResults?: ExtensionCommandResult[] | undefined;
  discoveredCommands?: DiscoveredExtensionCommand[] | undefined;
} = {}): {
  client: Ref<unknown>;
  submitCalls: SubmitCall[];
  invokeCalls: InvokeCall[];
} {
  const submitCalls: SubmitCall[] = [];
  const invokeCalls: InvokeCall[] = [];
  const queue = [...executeResults];
  const submitQueue = [...(options.submitResults ?? [])];
  const fake = {
    async invokeExtensionCommand(command: DiscoveredExtensionCommand, roomJid?: string): Promise<ExtensionCommandResult> {
      invokeCalls.push({ command, roomJid });
      if (options.invokeError) throw options.invokeError;
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
      return submitQueue.shift() ?? { status: "completed", notes: [] };
    },
    async discoverExtensionCommands(): Promise<DiscoveredExtensionCommand[]> {
      return options.discoveredCommands ?? [];
    },
  };
  return { client: ref(fake), submitCalls, invokeCalls };
}

function createLauncher(
  executeResults: ExtensionCommandResult[],
  options: {
    roomJid?: string | null;
    sendPublicChannelMessage?: (body: string) => Promise<void>;
    invokeExtensionAction?: (action: ExtensionAnnotationAction) => Promise<ExtensionCommandResult>;
    events?: string[] | undefined;
    invokeError?: Error | undefined;
    submitError?: Error | undefined;
    submitResults?: ExtensionCommandResult[] | undefined;
    discoveredCommands?: DiscoveredExtensionCommand[] | undefined;
  } = {},
) {
  const roomJid = ref(options.roomJid ?? null);
  const fake = fakeClient(executeResults, {
    events: options.events,
    invokeError: options.invokeError,
    submitError: options.submitError,
    submitResults: options.submitResults,
    discoveredCommands: options.discoveredCommands,
  });
  const launcher = useExtensionLauncher({
    xmppClient: fake.client as never,
    roomJid,
    invokeExtensionAction: ref(options.invokeExtensionAction),
    sendPublicChannelMessage: ref(options.sendPublicChannelMessage),
    focusPalette: () => {},
    focusComposerExtensions: () => {},
  });
  return { ...fake, launcher, roomJid };
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
    expect(invokeCalls).toEqual([{ command: aiCommand, roomJid: undefined }]);
    expect(submitCalls).toHaveLength(1);
    expect(submitCalls[0].action).toBe("complete");
    expect(submitCalls[0].fields.find((f) => f.name === "prompt")?.values).toEqual([
      "tell me a joke",
    ]);
    expect(launcher.open.value).toBe(false);
  });

  test("passes the active room into XEP-0050 inline command submissions", async () => {
    const { launcher, submitCalls, invokeCalls } = createLauncher([
      executingResult([field("prompt", { required: true })], ["complete", "cancel"]),
    ], { roomJid: "pub@muc.example.com" });

    await launcher.dispatchSlashInvocation({
      kind: "inline-submit",
      command: aiCommand,
      fieldName: "prompt",
      value: "tell the room",
    });

    expect(invokeCalls[0]).toEqual({ command: aiCommand, roomJid: "pub@muc.example.com" });
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
    expect(launcher.open.value).toBe(true);
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
    expect(launcher.open.value).toBe(true);
  });

  test("opens the palette when inline command invocation fails", async () => {
    const { launcher } = createLauncher([], {
      invokeError: new Error("invoke failed"),
    });

    const ok = await launcher.dispatchSlashInvocation({
      kind: "inline-submit",
      command: aiCommand,
      fieldName: "prompt",
      value: "tell me a joke",
    });

    expect(ok).toBe(false);
    expect(launcher.commandStates.value[aiCommand.node]).toEqual({
      state: "error",
      detail: "invoke failed",
    });
    expect(launcher.open.value).toBe(true);
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

  test("falls back to the palette when the executing form carries a forbidden field", async () => {
    const events: string[] = [];
    const { launcher, submitCalls } = createLauncher([
      executingResult([field("prompt"), field("payload#api_key", { label: "API key" })]),
    ], {
      events,
      roomJid: "pub@muc.example.com",
      sendPublicChannelMessage: async (body) => {
        events.push(`public:${body}`);
      },
    });

    const ok = await launcher.dispatchSlashInvocation({
      kind: "inline-submit",
      command: aiCommand,
      fieldName: "prompt",
      value: "hello",
    });

    // Nothing reaches the wire (no submit, no public /ai prompt); the
    // palette opens showing the blocked reason with submit disabled.
    expect(ok).toBe(true);
    expect(submitCalls).toEqual([]);
    expect(events.filter((event) => event.startsWith("public:"))).toEqual([]);
    expect(launcher.open.value).toBe(true);
  });

  test("falls back to the palette when a required field is still empty", async () => {
    const { launcher, submitCalls } = createLauncher([
      executingResult([field("prompt"), field("visibility", { required: true })]),
    ]);

    const ok = await launcher.dispatchSlashInvocation({
      kind: "inline-submit",
      command: aiCommand,
      fieldName: "prompt",
      value: "hello",
    });

    expect(ok).toBe(true);
    expect(submitCalls).toEqual([]);
    expect(launcher.open.value).toBe(true);
    const stored = launcher.commandForms.value[aiCommand.node]?.fields.find((f) => f.name === "prompt");
    expect(stored?.values).toEqual(["hello"]);
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
    expect(launcher.open.value).toBe(false);
  });

  test("opens the palette when the initial inline invoke returns a warning", async () => {
    const { launcher, submitCalls } = createLauncher([
      {
        status: "completed",
        notes: [{ type: "warning", value: "inline command needs attention" }],
      },
    ]);

    const ok = await launcher.dispatchSlashInvocation({
      kind: "inline-submit",
      command: aiCommand,
      fieldName: "prompt",
      value: "tell me a joke",
    });

    expect(ok).toBe(true);
    expect(submitCalls).toEqual([]);
    expect(launcher.commandStates.value[aiCommand.node]).toEqual({
      state: "warning",
      detail: "inline command needs attention",
    });
    expect(launcher.open.value).toBe(true);
  });

  test("opens the palette when inline submit returns a warning result", async () => {
    const { launcher, submitCalls } = createLauncher([
      executingResult([field("prompt", { required: true })], ["complete", "cancel"]),
    ], {
      submitResults: [{
        status: "completed",
        notes: [{ type: "warning", value: "submit needs attention" }],
      }],
    });

    const ok = await launcher.dispatchSlashInvocation({
      kind: "inline-submit",
      command: aiCommand,
      fieldName: "prompt",
      value: "tell me a joke",
    });

    expect(ok).toBe(true);
    expect(submitCalls).toHaveLength(1);
    expect(launcher.commandStates.value[aiCommand.node]).toEqual({
      state: "warning",
      detail: "submit needs attention",
    });
    expect(launcher.open.value).toBe(true);
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
    expect(launcher.open.value).toBe(true);
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
    expect(launcher.open.value).toBe(true);
  });
});

describe("dispatchSlashInvocation: direct-execute", () => {
  test("invokes completed channel commands without opening the palette", async () => {
    const { launcher, invokeCalls, submitCalls } = createLauncher([
      { status: "completed", notes: [] },
    ], { roomJid: "pub@muc.example.com" });

    const ok = await launcher.dispatchSlashInvocation({
      kind: "direct-execute",
      command: stargateCommand,
    });

    expect(ok).toBe(true);
    expect(invokeCalls).toEqual([{ command: stargateCommand, roomJid: "pub@muc.example.com" }]);
    expect(submitCalls).toEqual([]);
    expect(launcher.open.value).toBe(false);
    expect(launcher.commandStates.value[stargateCommand.node]?.state).toBe("success");
  });

  test("closes an already-open palette after a clean no-form result", async () => {
    const { launcher } = createLauncher([
      { status: "completed", notes: [] },
    ], { roomJid: "pub@muc.example.com" });
    launcher.open.value = true;

    const ok = await launcher.dispatchSlashInvocation({
      kind: "direct-execute",
      command: stargateCommand,
    });

    expect(ok).toBe(true);
    expect(launcher.open.value).toBe(false);
  });

  test("opens the palette for a no-form warning result", async () => {
    const { launcher } = createLauncher([
      {
        status: "completed",
        notes: [{ type: "warning", value: "Stargate quotes require an active channel." }],
      },
    ]);

    const ok = await launcher.dispatchSlashInvocation({
      kind: "direct-execute",
      command: stargateCommand,
    });

    expect(ok).toBe(true);
    expect(launcher.open.value).toBe(true);
    expect(launcher.commandStates.value[stargateCommand.node]).toEqual({
      state: "warning",
      detail: "Stargate quotes require an active channel.",
    });
  });

  test("falls back to the palette when direct execution returns a form", async () => {
    const { launcher } = createLauncher([
      executingResult([field("question", { required: true })]),
    ], { roomJid: "pub@muc.example.com" });

    const ok = await launcher.dispatchSlashInvocation({
      kind: "direct-execute",
      command: stargateCommand,
    });

    expect(ok).toBe(true);
    expect(launcher.open.value).toBe(true);
    expect(launcher.commandForms.value[stargateCommand.node]?.fields[0]?.name).toBe("question");
  });

  test("opens the palette when direct execution fails", async () => {
    const { launcher, invokeCalls } = createLauncher([], {
      roomJid: "pub@muc.example.com",
      invokeError: new Error("command failed"),
    });

    const ok = await launcher.dispatchSlashInvocation({
      kind: "direct-execute",
      command: stargateCommand,
    });

    expect(ok).toBe(false);
    expect(invokeCalls).toEqual([{ command: stargateCommand, roomJid: "pub@muc.example.com" }]);
    expect(launcher.open.value).toBe(true);
    expect(launcher.commandStates.value[stargateCommand.node]).toEqual({
      state: "error",
      detail: "command failed",
    });
  });
});

describe("dispatchSlashInvocation: open-palette", () => {
  test("passes the active room into initial execute for single-stage channel commands", async () => {
    const { launcher, invokeCalls, submitCalls } = createLauncher([
      {
        status: "completed",
        notes: [{ type: "info", value: "Stargate quote posted to channel." }],
      },
    ], { roomJid: "pub@muc.example.com" });

    const ok = await launcher.dispatchSlashInvocation({
      kind: "open-palette",
      command: stargateCommand,
    });

    expect(ok).toBe(true);
    expect(invokeCalls).toEqual([{ command: stargateCommand, roomJid: "pub@muc.example.com" }]);
    expect(submitCalls).toEqual([]);
    expect(launcher.open.value).toBe(false);
    expect(launcher.commandStates.value[stargateCommand.node]).toEqual({
      state: "success",
      detail: "Stargate quote posted to channel.",
    });
  });

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

describe("invokeCommand", () => {
  test("passes the active room into direct palette command execution", async () => {
    const { launcher, invokeCalls } = createLauncher([
      { status: "completed", notes: [] },
    ], { roomJid: "pub@muc.example.com" });

    await launcher.invokeCommand(stargateCommand);

    expect(invokeCalls).toEqual([{ command: stargateCommand, roomJid: "pub@muc.example.com" }]);
    expect(launcher.commandStates.value[stargateCommand.node]?.state).toBe("success");
  });

  test("closes the palette after a clean no-form command execution", async () => {
    const { launcher } = createLauncher([
      { status: "completed", notes: [] },
    ], { roomJid: "pub@muc.example.com" });
    launcher.open.value = true;

    await launcher.invokeCommand(stargateCommand);

    expect(launcher.open.value).toBe(false);
  });

  test("closes the palette after a no-form success result with an info note", async () => {
    const { launcher } = createLauncher([
      {
        status: "completed",
        notes: [{ type: "info", value: "Stargate quote posted to channel." }],
      },
    ], { roomJid: "pub@muc.example.com" });
    launcher.open.value = true;

    await launcher.invokeCommand(stargateCommand);

    expect(launcher.open.value).toBe(false);
    expect(launcher.commandStates.value[stargateCommand.node]).toEqual({
      state: "success",
      detail: "Stargate quote posted to channel.",
    });
  });

  test("keeps the palette open when command execution returns a form", async () => {
    const { launcher } = createLauncher([
      executingResult([field("question", { required: true })]),
    ], { roomJid: "pub@muc.example.com" });

    await launcher.invokeCommand(pollCommand);

    expect(launcher.open.value).toBe(true);
    expect(launcher.commandForms.value[pollCommand.node]?.fields[0]?.name).toBe("question");
  });

  test("opens the palette when command execution returns a warning", async () => {
    const { launcher } = createLauncher([
      {
        status: "completed",
        notes: [{ type: "warning", value: "command needs attention" }],
      },
    ], { roomJid: "pub@muc.example.com" });

    await launcher.invokeCommand(stargateCommand);

    expect(launcher.open.value).toBe(true);
    expect(launcher.commandStates.value[stargateCommand.node]).toEqual({
      state: "warning",
      detail: "command needs attention",
    });
  });

  test("opens the palette when command execution fails", async () => {
    const { launcher } = createLauncher([], {
      roomJid: "pub@muc.example.com",
      invokeError: new Error("command failed"),
    });

    await launcher.invokeCommand(stargateCommand);

    expect(launcher.open.value).toBe(true);
    expect(launcher.commandStates.value[stargateCommand.node]).toEqual({
      state: "error",
      detail: "command failed",
    });
  });

  test("ignores pending command execution results after a conversation reset", async () => {
    let resolveInvoke: ((result: ExtensionCommandResult) => void) | null = null;
    const invokeCalls: InvokeCall[] = [];
    const fake = {
      async invokeExtensionCommand(command: DiscoveredExtensionCommand, roomJid?: string): Promise<ExtensionCommandResult> {
        invokeCalls.push({ command, roomJid });
        return new Promise((resolve) => {
          resolveInvoke = resolve;
        });
      },
      async submitExtensionCommandForm(): Promise<ExtensionCommandResult> {
        return { status: "completed", notes: [] };
      },
      async discoverExtensionCommands(): Promise<DiscoveredExtensionCommand[]> {
        return [];
      },
    };
    const launcher = useExtensionLauncher({
      xmppClient: ref(fake) as never,
      roomJid: ref("room-a@muc.example.com"),
      invokeExtensionAction: ref(undefined),
      focusPalette: () => {},
      focusComposerExtensions: () => {},
    });

    const pending = launcher.dispatchSlashInvocation({
      kind: "open-palette",
      command: pollCommand,
    });
    launcher.reset();
    resolveInvoke?.(executingResult([field("question", { required: true })]));

    expect(await pending).toBe(false);
    expect(invokeCalls).toEqual([{ command: pollCommand, roomJid: "room-a@muc.example.com" }]);
    expect(launcher.commandForms.value).toEqual({});
    expect(launcher.commandStates.value).toEqual({});
  });
});

describe("invokeResultAction", () => {
  const action: ExtensionAnnotationAction = {
    label: "Finish",
    route: "urn:waddle:test-action",
  };

  test("closes the palette after a clean no-form action result", async () => {
    const { launcher } = createLauncher([], {
      invokeExtensionAction: async () => ({ status: "completed", notes: [] }),
    });
    launcher.open.value = true;
    launcher.commandActions.value = { [genericCommand.node]: [action] };

    await launcher.invokeResultAction(genericCommand, action);

    expect(launcher.open.value).toBe(false);
    expect(launcher.commandActions.value[genericCommand.node]).toBeUndefined();
    expect(launcher.commandStates.value[genericCommand.node]?.state).toBe("success");
  });

  test("opens the palette when an action invocation fails", async () => {
    const { launcher } = createLauncher([], {
      invokeExtensionAction: async () => {
        throw new Error("action failed");
      },
    });

    await launcher.invokeResultAction(genericCommand, action);

    expect(launcher.open.value).toBe(true);
    expect(launcher.commandStates.value[genericCommand.node]).toEqual({
      state: "error",
      detail: "action failed",
    });
  });

  test("opens the palette when an action result returns a warning", async () => {
    const { launcher } = createLauncher([], {
      invokeExtensionAction: async () => ({
        status: "completed",
        notes: [{ type: "warning", value: "action needs attention" }],
      }),
    });

    await launcher.invokeResultAction(genericCommand, action);

    expect(launcher.open.value).toBe(true);
    expect(launcher.commandStates.value[genericCommand.node]).toEqual({
      state: "warning",
      detail: "action needs attention",
    });
  });

  test("keeps the palette open when an action result returns a follow-up form", async () => {
    const { launcher } = createLauncher([], {
      invokeExtensionAction: async () =>
        executingResult([field("confirm", { required: true })], ["complete", "cancel"]),
    });

    await launcher.invokeResultAction(genericCommand, action);

    expect(launcher.open.value).toBe(true);
    expect(launcher.commandForms.value[genericCommand.node]?.fields[0]?.name).toBe("confirm");
  });

  test("keeps the palette open when an action result returns launch actions", async () => {
    const { launcher } = createLauncher([], {
      invokeExtensionAction: async () => launchResult(),
    });

    await launcher.invokeResultAction(genericCommand, action);

    expect(launcher.open.value).toBe(true);
    expect(launcher.commandActions.value[genericCommand.node]?.[0]).toMatchObject({
      label: "Finish",
      route: "route-1",
    });
  });
});

describe("extension command discovery lifecycle", () => {
  test("keeps discovered commands across conversation UI resets", async () => {
    const { launcher } = createLauncher([], {
      roomJid: "pub@muc.example.com",
      discoveredCommands: [stargateCommand],
    });

    await launcher.ensureDiscovered();
    expect(launcher.commands.value).toEqual([stargateCommand]);
    expect(launcher.availableCommands.value).toEqual([stargateCommand]);

    launcher.reset();

    expect(launcher.commands.value).toEqual([stargateCommand]);
    expect(launcher.availableCommands.value).toEqual([stargateCommand]);
    expect(launcher.open.value).toBe(false);
    expect(launcher.commandStates.value).toEqual({});
  });

  test("clears discovered commands when explicitly resetting the session", async () => {
    const { launcher } = createLauncher([], {
      discoveredCommands: [stargateCommand],
    });

    await launcher.ensureDiscovered();
    launcher.reset({ clearCommands: true });

    expect(launcher.commands.value).toEqual([]);
  });

  test("retries discovery after an empty result", async () => {
    const discovered = [
      [],
      [stargateCommand],
    ];
    const fake = {
      async discoverExtensionCommands(): Promise<DiscoveredExtensionCommand[]> {
        return discovered.shift() ?? [];
      },
      async invokeExtensionCommand(): Promise<ExtensionCommandResult> {
        return { status: "completed", notes: [] };
      },
      async submitExtensionCommandForm(): Promise<ExtensionCommandResult> {
        return { status: "completed", notes: [] };
      },
    };
    const launcher = useExtensionLauncher({
      xmppClient: ref(fake) as never,
      roomJid: ref("pub@muc.example.com"),
      invokeExtensionAction: ref(undefined),
      focusPalette: () => {},
      focusComposerExtensions: () => {},
    });

    await launcher.ensureDiscovered();
    expect(launcher.commands.value).toEqual([]);

    await launcher.ensureDiscovered();
    expect(launcher.commands.value).toEqual([stargateCommand]);
    expect(launcher.availableCommands.value).toEqual([stargateCommand]);
  });

  test("ignores stale discovery results after a session reset", async () => {
    let resolveDiscovery: ((commands: DiscoveredExtensionCommand[]) => void) | null = null;
    const fake = {
      async discoverExtensionCommands(): Promise<DiscoveredExtensionCommand[]> {
        return new Promise((resolve) => {
          resolveDiscovery = resolve;
        });
      },
      async invokeExtensionCommand(): Promise<ExtensionCommandResult> {
        return { status: "completed", notes: [] };
      },
      async submitExtensionCommandForm(): Promise<ExtensionCommandResult> {
        return { status: "completed", notes: [] };
      },
    };
    const launcher = useExtensionLauncher({
      xmppClient: ref(fake) as never,
      roomJid: ref("pub@muc.example.com"),
      invokeExtensionAction: ref(undefined),
      focusPalette: () => {},
      focusComposerExtensions: () => {},
    });

    const pendingDiscovery = launcher.ensureDiscovered();
    launcher.reset({ clearCommands: true });
    resolveDiscovery?.([stargateCommand]);
    await pendingDiscovery;

    expect(launcher.commands.value).toEqual([]);
    expect(launcher.availableCommands.value).toEqual([]);
  });
});

describe("submitForm: public channel output", () => {
  test("uses the room captured when the command form was opened", async () => {
    const { launcher, submitCalls, roomJid } = createLauncher([
      executingResult([field("prompt", { required: true })], ["complete", "cancel"]),
    ], { roomJid: "room-a@muc.example.com" });
    await launcher.invokeCommand(aiCommand);
    expect(launcher.open.value).toBe(true);
    roomJid.value = "room-b@muc.example.com";
    launcher.updateField(aiCommand.node, "prompt", ["stay in room a"]);

    const ok = await launcher.submitForm(aiCommand, "complete", { skipPublicPrompt: true });

    expect(ok).toBe(true);
    expect(submitCalls).toHaveLength(1);
    expect(submitCalls[0].roomJid).toBe("room-a@muc.example.com");
    expect(launcher.open.value).toBe(false);
  });

  test("keeps the palette open when submission returns a follow-up form", async () => {
    const { launcher } = createLauncher([
      executingResult([field("prompt", { required: true })], ["complete", "cancel"]),
    ], {
      submitResults: [
        executingResult([field("confirm", { required: true })], ["complete", "cancel"]),
      ],
    });
    await launcher.invokeCommand(aiCommand);
    launcher.updateField(aiCommand.node, "prompt", ["continue"]);

    const ok = await launcher.submitForm(aiCommand, "complete", { skipPublicPrompt: true });

    expect(ok).toBe(true);
    expect(launcher.open.value).toBe(true);
    expect(launcher.commandForms.value[aiCommand.node]?.fields[0]?.name).toBe("confirm");
  });

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
    expect(launcher.open.value).toBe(false);
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
    expect(launcher.open.value).toBe(true);
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
    expect(launcher.open.value).toBe(false);
  });
});
