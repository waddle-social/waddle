import { computed, nextTick, ref, type Ref } from "vue";
import type { ExtensionAnnotationAction } from "@/lib/chat-ui";
import type { BrowserXmppClient } from "@/lib/xmpp-client";
import {
  extensionCommandFormBlockedReason,
  extensionCommandOutcome,
  missingRequiredExtensionCommandFields,
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
  const commandForms = ref<Record<string, { sessionId: string; fields: ExtensionCommandFormField[]; actions?: ExtensionCommandAction[]; roomJid?: string }>>({});
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

  function storeResultSurfaces(key: string, result: ExtensionCommandResult, options: { roomJid?: string } = {}) {
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
            ...(options.roomJid ? { roomJid: options.roomJid } : {}),
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

  function syncPaletteForCommandOutcome(
    key: string,
    outcome: { state: ExtensionCommandUiState; detail?: string },
  ) {
    const hasResultSurface = !!commandForms.value[key] || (commandActions.value[key]?.length ?? 0) > 0;
    if (hasResultSurface || outcome.state !== "success") {
      open.value = true;
      void nextTick(input.focusPalette);
      return;
    }
    open.value = false;
  }

  let discoveryPromise: Promise<void> | null = null;
  let discoveryAttempted = false;
  let discoveryGeneration = 0;
  let commandGeneration = 0;

  function commandContext() {
    return {
      generation: commandGeneration,
      roomJid: input.roomJid.value ?? undefined,
    };
  }

  function isCurrentCommandContext(context: { generation: number }) {
    return context.generation === commandGeneration;
  }

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
    const generation = discoveryGeneration;
    discoveryPromise = (async () => {
      try {
        const discovered = await client.discoverExtensionCommands();
        if (generation !== discoveryGeneration) return;
        commands.value = discovered;
        discoveryAttempted = commands.value.length > 0;
        state.value = "idle";
        if (commands.value.length === 0) detail.value = "No extension commands discovered.";
      } catch (error) {
        if (generation !== discoveryGeneration) return;
        state.value = "error";
        detail.value = error instanceof Error ? error.message : "Could not discover extension commands.";
      } finally {
        if (generation === discoveryGeneration) discoveryPromise = null;
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
    const context = commandContext();
    clearCommandSurfaces(key);
    commandStates.value = { ...commandStates.value, [key]: { state: "loading" } };
    try {
      const result = await client.invokeExtensionCommand(command, context.roomJid);
      if (!isCurrentCommandContext(context)) return;
      storeResultSurfaces(key, result, { roomJid: context.roomJid });
      const outcome = extensionCommandOutcome(result);
      commandStates.value = { ...commandStates.value, [key]: outcome };
      syncPaletteForCommandOutcome(key, outcome);
    } catch (error) {
      if (!isCurrentCommandContext(context)) return;
      commandStates.value = {
        ...commandStates.value,
        [key]: { state: "error", detail: error instanceof Error ? error.message : "Extension command failed." },
      };
      open.value = true;
      void nextTick(input.focusPalette);
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

  function reset(options: { clearCommands?: boolean } = {}) {
    commandGeneration += 1;
    open.value = false;
    state.value = "idle";
    detail.value = "";
    commandStates.value = {};
    commandForms.value = {};
    commandActions.value = {};
    if (options.clearCommands) {
      discoveryGeneration += 1;
      discoveryPromise = null;
      commands.value = [];
      discoveryAttempted = false;
    }
  }

  async function submitForm(
    command: DiscoveredExtensionCommand,
    action: ExtensionCommandAction = "complete",
    options: { skipPublicPrompt?: boolean; roomJid?: string; generation?: number } = {},
  ): Promise<boolean> {
    const client = input.xmppClient.value;
    if (!client) return false;
    const key = command.node;
    const form = commandForms.value[key];
    if (!form) return false;
    const context = {
      generation: options.generation ?? commandGeneration,
      roomJid: options.roomJid ?? form.roomJid,
    };
    // Hard gate (the palette also disables submit): a form carrying a
    // forbidden field must never reach the wire with values, and
    // required fields must be filled for any forward action. Runs
    // BEFORE the AI public prompt so a refused submit posts nothing.
    if (action !== "cancel" && action !== "prev") {
      const blocked = extensionCommandFormBlockedReason(form.fields);
      const gateDetail = blocked ??
        (missingRequiredExtensionCommandFields(form.fields).length > 0
          ? "Complete required fields to continue."
          : undefined);
      if (gateDetail) {
        commandStates.value = { ...commandStates.value, [key]: { state: "error", detail: gateDetail } };
        open.value = true;
        void nextTick(input.focusPalette);
        return false;
      }
    }
    commandStates.value = { ...commandStates.value, [key]: { state: "loading" } };
    try {
      if (!options.skipPublicPrompt) {
        if (!isCurrentCommandContext(context)) return false;
        await sendPublicPromptIfRequested(command, form.fields, action, context.roomJid);
      }
      if (!isCurrentCommandContext(context)) return false;
      const result = await client.submitExtensionCommandForm(command, form.sessionId, form.fields, action, context.roomJid);
      if (!isCurrentCommandContext(context)) return false;
      storeResultSurfaces(key, result, { roomJid: context.roomJid });
      const outcome = extensionCommandOutcome(result);
      commandStates.value = { ...commandStates.value, [key]: outcome };
      syncPaletteForCommandOutcome(key, outcome);
      return true;
    } catch (error) {
      if (!isCurrentCommandContext(context)) return false;
      commandStates.value = {
        ...commandStates.value,
        [key]: { state: "error", detail: error instanceof Error ? error.message : "Extension form submission failed." },
      };
      open.value = true;
      void nextTick(input.focusPalette);
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
    const context = commandContext();

    clearCommandSurfaces(key);
    commandStates.value = { ...commandStates.value, [key]: { state: "loading" } };
    if (wantsPalette) {
      open.value = true;
      void nextTick(input.focusPalette);
    }

    try {
      const result = await client.invokeExtensionCommand(command, context.roomJid);
      if (!isCurrentCommandContext(context)) return false;
      storeResultSurfaces(key, result, { roomJid: context.roomJid });
      const form = commandForms.value[key];

      if (invocation.kind === "open-palette") {
        const outcome = extensionCommandOutcome(result);
        commandStates.value = { ...commandStates.value, [key]: outcome };
        if (invocation.prefillFirstRequired && form) {
          const target = form.fields.find((field) => field.required && !field.hidden);
          if (target) {
            updateField(key, target.name, [invocation.prefillFirstRequired]);
          }
        }
        syncPaletteForCommandOutcome(key, outcome);
        return true;
      }

      if (invocation.kind === "direct-execute") {
        const outcome = extensionCommandOutcome(result);
        commandStates.value = { ...commandStates.value, [key]: outcome };
        syncPaletteForCommandOutcome(key, outcome);
        return true;
      }

      // inline-submit only stays inline if the server returned a single-stage
      // form whose advertised XEP-0050 actions include `complete`, with no
      // forbidden field and every required field filled. Otherwise fall back
      // to the palette so the user can drive the multi-stage flow (and see
      // the blocked reason / required gating with submit disabled).
      const allowed = form?.actions ?? [];
      if (form && allowed.includes("complete")) {
        updateField(key, invocation.fieldName, [invocation.value]);
        forceChannelOutputForSlashAi(command, context.roomJid);
        const fields = commandForms.value[key]?.fields ?? [];
        const safeToSubmit = !extensionCommandFormBlockedReason(fields) &&
          missingRequiredExtensionCommandFields(fields).length === 0;
        if (safeToSubmit) {
          return await submitForm(command, "complete", { roomJid: context.roomJid, generation: context.generation });
        }
      }

      const outcome = extensionCommandOutcome(result);
      commandStates.value = { ...commandStates.value, [key]: outcome };
      if (form) {
        updateField(key, invocation.fieldName, [invocation.value]);
      }
      syncPaletteForCommandOutcome(key, outcome);
      return true;
    } catch (error) {
      if (!isCurrentCommandContext(context)) return false;
      commandStates.value = {
        ...commandStates.value,
        [key]: { state: "error", detail: error instanceof Error ? error.message : "Extension command failed." },
      };
      open.value = true;
      void nextTick(input.focusPalette);
      return false;
    }
  }

  async function sendPublicPromptIfRequested(
    command: DiscoveredExtensionCommand,
    fields: ExtensionCommandFormField[],
    action: ExtensionCommandAction,
    roomJid?: string,
  ) {
    if (command.node !== AI_CHATBOT_COMMAND_NODE) return;
    if (action === "cancel" || action === "prev") return;
    if (formFieldValue(fields, "output") !== "channel") return;
    if (!roomJid) return;
    const prompt = formFieldValue(fields, "prompt")?.trim();
    if (!prompt) return;
    const sendPublicChannelMessage = input.sendPublicChannelMessage?.value;
    if (!sendPublicChannelMessage) {
      throw new Error("Cannot post the AI prompt to this channel.");
    }
    await sendPublicChannelMessage(prompt);
  }

  function forceChannelOutputForSlashAi(command: DiscoveredExtensionCommand, roomJid?: string) {
    if (!roomJid || command.node !== AI_CHATBOT_COMMAND_NODE) return;
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
    const context = commandContext();
    commandStates.value = { ...commandStates.value, [key]: { state: "loading" } };
    try {
      const result = await invokeExtensionAction(action);
      if (!isCurrentCommandContext(context)) return;
      storeResultSurfaces(key, result, { roomJid: context.roomJid });
      const outcome = extensionCommandOutcome(result);
      commandStates.value = { ...commandStates.value, [key]: outcome };
      syncPaletteForCommandOutcome(key, outcome);
    } catch (error) {
      if (!isCurrentCommandContext(context)) return;
      commandStates.value = {
        ...commandStates.value,
        [key]: { state: "error", detail: error instanceof Error ? error.message : "Extension action failed." },
      };
      open.value = true;
      void nextTick(input.focusPalette);
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
