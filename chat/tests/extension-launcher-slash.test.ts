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
      })),
    },
  };
}

function fakeClient(executeResults: ExtensionCommandResult[]): {
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
      submitCalls.push({ command, sessionId, fields, action, roomJid });
      return { status: "completed", notes: [] };
    },
    async discoverExtensionCommands(): Promise<DiscoveredExtensionCommand[]> {
      return [];
    },
  };
  return { client: ref(fake), submitCalls, invokeCalls };
}

function createLauncher(executeResults: ExtensionCommandResult[]) {
  const fake = fakeClient(executeResults);
  const launcher = useExtensionLauncher({
    xmppClient: fake.client as never,
    roomJid: ref(null),
    invokeExtensionAction: ref(undefined),
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
    await launcher.dispatchSlashInvocation(invocation);

    expect(invokeCalls).toEqual([aiCommand]);
    expect(submitCalls).toHaveLength(1);
    expect(submitCalls[0].action).toBe("complete");
    expect(submitCalls[0].fields.find((f) => f.name === "prompt")?.values).toEqual([
      "tell me a joke",
    ]);
    expect(launcher.open.value).toBe(false);
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
