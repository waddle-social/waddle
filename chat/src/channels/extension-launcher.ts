import { computed, nextTick, ref, type Ref } from "vue";
import type { ExtensionAnnotationAction } from "@/lib/chat-ui";
import type { BrowserXmppClient } from "@/lib/xmpp-client";
import {
  extensionCommandOutcome,
  parseExtensionCommandForm,
  parseExtensionCommandLaunches,
  type DiscoveredExtensionCommand,
  type ExtensionCommandAction,
  type ExtensionCommandFormField,
  type ExtensionCommandResult,
} from "@/lib/xmpp/extension-commands";
import type { SlashInvocation } from "@/lib/slash-dispatch";

type ExtensionLauncherState = "idle" | "loading" | "error";
type ExtensionCommandUiState = "loading" | "success" | "warning" | "error";
type ReadonlyRef<T> = Readonly<Ref<T>>;
const AI_CHATBOT_COMMAND_NODE = "urn:waddle:extension:1:ai-chatbot";

export function useExtensionLauncher(input: {
  xmppClient: ReadonlyRef<BrowserXmppClient | null | undefined>;
  roomJid: ReadonlyRef<string | null | undefined>;
  invokeExtensionAction: ReadonlyRef<((action: ExtensionAnnotationAction) => Promise<ExtensionCommandResult>) | undefined>;
  sendPublicChannelMessage?: ReadonlyRef<((body: string) => Promise<void>) | undefined>;
  focusPalette: () => void;
  focusComposerExtensions: () => void;
}) {
  const open = ref(false);
  const commands = ref<DiscoveredExtensionCommand[]>([]);
  const state = ref<ExtensionLauncherState>("idle");
  const detail = ref("");
  const commandStates = ref<Record<string, { state: ExtensionCommandUiState; detail?: string }>>({});
  const commandForms = ref<Record<string, { sessionId: string; fields: ExtensionCommandFormField[]; actions?: ExtensionCommandAction[] }>>({});
  const commandActions = ref<Record<string, ExtensionAnnotationAction[]>>({});
  const availableCommands = computed(() =>
    input.roomJid.value
      ? commands.value
      : commands.value.filter((command) => command.scope !== "channel"),
  );

  function close() {
    open.value = false;
    void nextTick(input.focusComposerExtensions);
  }

  function clearCommandSurfaces(key: string) {
    const nextForms = { ...commandForms.value };
    delete nextForms[key];
    commandForms.value = nextForms;
    const nextActions = { ...commandActions.value };
    delete nextActions[key];
    commandActions.value = nextActions;
  }

  function storeResultSurfaces(key: string, result: ExtensionCommandResult) {
    let foundActions = false;
    if (result.form) {
      const actions = parseExtensionCommandLaunches(result.form);
      if (actions.length > 0) {
        foundActions = true;
        commandActions.value = { ...commandActions.value, [key]: actions };
      }
    }
    if (!foundActions) {
      const nextActions = { ...commandActions.value };
      delete nextActions[key];
      commandActions.value = nextActions;
    }

    if (result.sessionId && result.form && result.status === "executing") {
      const fields = parseExtensionCommandForm(result.form);
      if (fields.length > 0) {
        commandForms.value = {
          ...commandForms.value,
          [key]: {
            sessionId: result.sessionId,
            fields,
            ...(result.actions?.allowed.length ? { actions: result.actions.allowed } : {}),
          },
        };
        return;
      }
    }
    const nextForms = { ...commandForms.value };
    delete nextForms[key];
    commandForms.value = nextForms;
  }

  let discoveryPromise: Promise<void> | null = null;
  let discoveryAttempted = false;
  async function ensureDiscovered() {
    if (commands.value.length > 0 || discoveryAttempted) return;
    if (discoveryPromise) {
      await discoveryPromise;
      return;
    }
    const client = input.xmppClient.value;
    if (!client) return;
    state.value = "loading";
    detail.value = "";
    discoveryPromise = (async () => {
      try {
        commands.value = await client.discoverExtensionCommands();
        discoveryAttempted = true;
        state.value = "idle";
        if (commands.value.length === 0) detail.value = "No extension commands discovered.";
      } catch (error) {
        state.value = "error";
        detail.value = error instanceof Error ? error.message : "Could not discover extension commands.";
      } finally {
        discoveryPromise = null;
      }
    })();
    await discoveryPromise;
  }

  async function toggle() {
    open.value = !open.value;
    if (open.value) void nextTick(input.focusPalette);
    if (!open.value) return;
    const client = input.xmppClient.value;
    if (!client) {
      state.value = "error";
      detail.value = "Extensions are unavailable while XMPP is disconnected.";
      return;
    }
    await ensureDiscovered();
  }

  async function invokeCommand(command: DiscoveredExtensionCommand) {
    const client = input.xmppClient.value;
    if (!client) return;
    const key = command.node;
    clearCommandSurfaces(key);
    commandStates.value = { ...commandStates.value, [key]: { state: "loading" } };
    try {
      const result = await client.invokeExtensionCommand(command);
      storeResultSurfaces(key, result);
      commandStates.value = { ...commandStates.value, [key]: extensionCommandOutcome(result) };
    } catch (error) {
      commandStates.value = {
        ...commandStates.value,
        [key]: { state: "error", detail: error instanceof Error ? error.message : "Extension command failed." },
      };
    }
  }

  function updateField(commandNode: string, fieldName: string, values: string[]) {
    const form = commandForms.value[commandNode];
    if (!form) return;
    commandForms.value = {
      ...commandForms.value,
      [commandNode]: {
        ...form,
        fields: form.fields.map((field) =>
          field.name === fieldName
            ? { ...field, values, value: values[0] ?? "" }
            : field,
        ),
      },
    };
  }

  function reset() {
    open.value = false;
    commands.value = [];
    state.value = "idle";
    detail.value = "";
    commandStates.value = {};
    commandForms.value = {};
    commandActions.value = {};
    discoveryAttempted = false;
  }

  async function submitForm(
    command: DiscoveredExtensionCommand,
    action: ExtensionCommandAction = "complete",
    options: { skipPublicPrompt?: boolean } = {},
  ): Promise<boolean> {
    const client = input.xmppClient.value;
    if (!client) return false;
    const key = command.node;
    const form = commandForms.value[key];
    if (!form) return false;
    commandStates.value = { ...commandStates.value, [key]: { state: "loading" } };
    try {
      if (!options.skipPublicPrompt) {
        await sendPublicPromptIfRequested(command, form.fields, action);
      }
      const result = await client.submitExtensionCommandForm(command, form.sessionId, form.fields, action, input.roomJid.value ?? undefined);
      storeResultSurfaces(key, result);
      commandStates.value = { ...commandStates.value, [key]: extensionCommandOutcome(result) };
      return true;
    } catch (error) {
      commandStates.value = {
        ...commandStates.value,
        [key]: { state: "error", detail: error instanceof Error ? error.message : "Extension form submission failed." },
      };
      return false;
    }
  }

  async function dispatchSlashInvocation(invocation: SlashInvocation): Promise<boolean> {
    const client = input.xmppClient.value;
    if (!client) {
      state.value = "error";
      detail.value = "Extensions are unavailable while XMPP is disconnected.";
      open.value = true;
      void nextTick(input.focusPalette);
      return false;
    }
    const command = invocation.command;
    const key = command.node;
    const wantsPalette = invocation.kind === "open-palette";

    clearCommandSurfaces(key);
    commandStates.value = { ...commandStates.value, [key]: { state: "loading" } };
    if (wantsPalette) {
      open.value = true;
      void nextTick(input.focusPalette);
    }

    try {
      const result = await client.invokeExtensionCommand(command);
      storeResultSurfaces(key, result);
      const form = commandForms.value[key];

      if (invocation.kind === "open-palette") {
        commandStates.value = { ...commandStates.value, [key]: extensionCommandOutcome(result) };
        if (invocation.prefillFirstRequired && form) {
          const target = form.fields.find((field) => field.required && !field.hidden);
          if (target) {
            updateField(key, target.name, [invocation.prefillFirstRequired]);
          }
        }
        return true;
      }

      // inline-submit only stays inline if the server returned a single-stage
      // form whose advertised XEP-0050 actions include `complete`. Otherwise
      // fall back to the palette so the user can drive the multi-stage flow.
      const allowed = form?.actions ?? [];
      if (form && allowed.includes("complete")) {
        updateField(key, invocation.fieldName, [invocation.value]);
        forceChannelOutputForSlashAi(command);
        return await submitForm(command, "complete");
      }

      commandStates.value = { ...commandStates.value, [key]: extensionCommandOutcome(result) };
      if (form) {
        updateField(key, invocation.fieldName, [invocation.value]);
        open.value = true;
        void nextTick(input.focusPalette);
      }
      return true;
    } catch (error) {
      commandStates.value = {
        ...commandStates.value,
        [key]: { state: "error", detail: error instanceof Error ? error.message : "Extension command failed." },
      };
      return false;
    }
  }

  async function sendPublicPromptIfRequested(
    command: DiscoveredExtensionCommand,
    fields: ExtensionCommandFormField[],
    action: ExtensionCommandAction,
  ) {
    if (command.node !== AI_CHATBOT_COMMAND_NODE) return;
    if (action === "cancel" || action === "prev") return;
    if (formFieldValue(fields, "output") !== "channel") return;
    if (!input.roomJid.value) return;
    const prompt = formFieldValue(fields, "prompt")?.trim();
    if (!prompt) return;
    const sendPublicChannelMessage = input.sendPublicChannelMessage?.value;
    if (!sendPublicChannelMessage) {
      throw new Error("Cannot post the AI prompt to this channel.");
    }
    await sendPublicChannelMessage(prompt);
  }

  function forceChannelOutputForSlashAi(command: DiscoveredExtensionCommand) {
    if (!input.roomJid.value || command.node !== AI_CHATBOT_COMMAND_NODE) return;
    const form = commandForms.value[command.node];
    const output = form?.fields.find((field) => field.name === "output");
    if (!output) return;
    const supportsChannelOutput = output.options.length === 0
      || output.options.some((option) => option.value === "channel");
    if (supportsChannelOutput) updateField(command.node, "output", ["channel"]);
  }

  function formFieldValue(
    fields: ExtensionCommandFormField[],
    name: string,
  ): string | undefined {
    const field = fields.find((candidate) => candidate.name === name);
    return field?.values.find((value) => value.trim().length > 0) ?? field?.value;
  }

  async function invokeResultAction(command: DiscoveredExtensionCommand, action: ExtensionAnnotationAction) {
    const invokeExtensionAction = input.invokeExtensionAction.value;
    if (!invokeExtensionAction) return;
    const key = command.node;
    commandStates.value = { ...commandStates.value, [key]: { state: "loading" } };
    try {
      const result = await invokeExtensionAction(action);
      storeResultSurfaces(key, result);
      commandStates.value = { ...commandStates.value, [key]: extensionCommandOutcome(result) };
    } catch (error) {
      commandStates.value = {
        ...commandStates.value,
        [key]: { state: "error", detail: error instanceof Error ? error.message : "Extension action failed." },
      };
    }
  }

  return {
    open,
    commands,
    state,
    detail,
    availableCommands,
    commandStates,
    commandForms,
    commandActions,
    close,
    toggle,
    ensureDiscovered,
    invokeCommand,
    updateField,
    reset,
    submitForm,
    invokeResultAction,
    dispatchSlashInvocation,
  };
}
