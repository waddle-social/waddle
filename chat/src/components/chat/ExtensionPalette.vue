<script setup lang="ts">
import { computed, ref } from "vue";
import {
  AlertCircle,
  ArrowLeft,
  ArrowRight,
  CheckCircle2,
  LoaderCircle,
  Play,
  ShieldCheck,
  Square,
  X,
} from "lucide-vue-next";
import {
  extensionCommandFormBlockedReason,
  missingRequiredExtensionCommandFields,
  visibleExtensionCommandFields,
  type DiscoveredExtensionCommand,
  type ExtensionCommandAction,
  type ExtensionCommandFormField,
} from "@/lib/xmpp/extension-commands";
import type { ExtensionAnnotationAction } from "@/lib/chat-ui";

const props = defineProps<{
  state: "idle" | "loading" | "error";
  detail: string;
  commands: DiscoveredExtensionCommand[];
  commandStates: Record<string, { state: "loading" | "success" | "warning" | "error"; detail?: string }>;
  commandForms: Record<string, { sessionId: string; fields: ExtensionCommandFormField[]; actions?: ExtensionCommandAction[] }>;
  commandActions: Record<string, ExtensionAnnotationAction[]>;
  isTopPinned?: boolean;
}>();

const emit = defineEmits<{
  close: [];
  invokeCommand: [command: DiscoveredExtensionCommand];
  submitForm: [command: DiscoveredExtensionCommand, action: ExtensionCommandAction];
  invokeAction: [command: DiscoveredExtensionCommand, action: ExtensionAnnotationAction];
  updateField: [commandNode: string, fieldName: string, values: string[]];
}>();

const adminCommands = computed(() =>
  props.commands.filter((command) => command.node.includes(":admin:")),
);
const memberCommands = computed(() =>
  props.commands.filter((command) => !command.node.includes(":admin:")),
);
const closeButtonRef = ref<HTMLButtonElement | null>(null);

function setCloseButtonRef(el: HTMLButtonElement | null) {
  closeButtonRef.value = el;
}

function focus() {
  closeButtonRef.value?.focus();
}

defineExpose({ focus });

function visibleFields(command: DiscoveredExtensionCommand): ExtensionCommandFormField[] {
  return visibleExtensionCommandFields(props.commandForms[command.node]?.fields ?? []);
}

function blockedReason(command: DiscoveredExtensionCommand): string | undefined {
  return extensionCommandFormBlockedReason(props.commandForms[command.node]?.fields ?? []);
}

function allowedActions(command: DiscoveredExtensionCommand): ExtensionCommandAction[] {
  const configured = props.commandForms[command.node]?.actions ?? [];
  const actions = configured.length > 0 ? configured : ["complete", "cancel"];
  return actions.includes("cancel") ? actions : [...actions, "cancel"];
}

function stateDetail(command: DiscoveredExtensionCommand): string {
  return props.commandStates[command.node]?.detail ?? "";
}

function missingRequiredFields(command: DiscoveredExtensionCommand): ExtensionCommandFormField[] {
  return missingRequiredExtensionCommandFields(props.commandForms[command.node]?.fields ?? []);
}

function canSubmitAction(command: DiscoveredExtensionCommand, action: ExtensionCommandAction): boolean {
  if (props.commandStates[command.node]?.state === "loading") return false;
  if (action === "cancel" || action === "prev") return true;
  return !blockedReason(command) && missingRequiredFields(command).length === 0;
}

function setSingleValue(command: DiscoveredExtensionCommand, field: ExtensionCommandFormField, value: string) {
  emit("updateField", command.node, field.name, [value]);
}

function setSingleValueFromEvent(command: DiscoveredExtensionCommand, field: ExtensionCommandFormField, event: Event) {
  setSingleValue(command, field, (event.target as HTMLInputElement | HTMLSelectElement).value);
}

function setBooleanValue(command: DiscoveredExtensionCommand, field: ExtensionCommandFormField, checked: boolean) {
  emit("updateField", command.node, field.name, [checked ? "1" : "0"]);
}

function setBooleanValueFromEvent(command: DiscoveredExtensionCommand, field: ExtensionCommandFormField, event: Event) {
  setBooleanValue(command, field, (event.target as HTMLInputElement).checked);
}

function toggleMultiValue(command: DiscoveredExtensionCommand, field: ExtensionCommandFormField, value: string, checked: boolean) {
  const values = new Set(field.values);
  if (checked) values.add(value);
  else values.delete(value);
  const orderedValues = field.options.length > 0
    ? field.options.map((option) => option.value).filter((optionValue) => values.has(optionValue))
    : field.values.filter((fieldValue) => values.has(fieldValue));
  emit("updateField", command.node, field.name, orderedValues);
}

function toggleMultiValueFromEvent(command: DiscoveredExtensionCommand, field: ExtensionCommandFormField, value: string, event: Event) {
  toggleMultiValue(command, field, value, (event.target as HTMLInputElement).checked);
}

function setMultiLineValueFromEvent(command: DiscoveredExtensionCommand, field: ExtensionCommandFormField, event: Event) {
  emit("updateField", command.node, field.name, (event.target as HTMLTextAreaElement).value.split("\n"));
}

function actionLabel(action: ExtensionCommandAction): string {
  switch (action) {
    case "prev":
      return "Back";
    case "next":
      return "Next";
    case "cancel":
      return "Cancel";
    default:
      return "Complete";
  }
}
</script>

<template>
  <section
    class="chat-extension-palette max-h-[min(72dvh,44rem)] overflow-y-auto overscroll-contain border-border bg-background/95 px-[var(--chat-content-inline)] py-3"
    :class="isTopPinned ? 'border-b' : 'border-t'"
    role="region"
    aria-label="Extensions"
  >
    <div class="chat-message-lane grid gap-3">
      <header class="flex items-center justify-between gap-3">
        <div class="min-w-0">
          <h2 class="type-card-title text-foreground">Extensions</h2>
          <p class="type-caption text-muted-foreground">
            Waddle-native actions available in this conversation.
          </p>
        </div>
        <button
          :ref="setCloseButtonRef"
          type="button"
          class="chat-icon-button h-9 w-9"
          title="Close extensions"
          aria-label="Close extensions"
          @click="emit('close')"
        >
          <X class="h-4 w-4" aria-hidden="true" />
        </button>
      </header>

      <div v-if="state === 'loading'" class="type-caption inline-flex items-center gap-2 text-muted-foreground" role="status">
        <LoaderCircle class="h-4 w-4 animate-spin" aria-hidden="true" />
        Loading extensions
      </div>

      <p v-else-if="state === 'error'" class="type-caption text-destructive" role="alert">
        {{ detail || "Could not load extensions." }}
      </p>

      <p v-else-if="commands.length === 0" class="type-caption text-muted-foreground">
        {{ detail || "No extensions are available here." }}
      </p>

      <div v-else class="grid gap-3">
        <div v-if="memberCommands.length > 0" class="grid gap-2">
          <h3 class="type-section-label text-muted-foreground">Available here</h3>
          <div class="flex flex-wrap gap-2">
            <button
              v-for="command in memberCommands"
              :key="command.node"
              type="button"
              class="type-control inline-flex min-h-10 max-w-full items-center gap-2 rounded-md border border-border bg-muted px-3 py-2 text-foreground transition-colors hover:bg-muted/70 disabled:opacity-60"
              :disabled="commandStates[command.node]?.state === 'loading'"
              @click="emit('invokeCommand', command)"
            >
              <LoaderCircle
                v-if="commandStates[command.node]?.state === 'loading'"
                class="h-4 w-4 animate-spin"
                aria-hidden="true"
              />
              <CheckCircle2
                v-else-if="commandStates[command.node]?.state === 'success'"
                class="h-4 w-4 text-success"
                aria-hidden="true"
              />
              <AlertCircle
                v-else-if="commandStates[command.node]?.state === 'warning' || commandStates[command.node]?.state === 'error'"
                class="h-4 w-4"
                :class="commandStates[command.node]?.state === 'error' ? 'text-destructive' : 'text-warning'"
                aria-hidden="true"
              />
              <Play v-else class="h-4 w-4 text-primary" aria-hidden="true" />
              <span class="min-w-0 break-words text-left">{{ command.name }}</span>
            </button>
          </div>
        </div>

        <div v-if="adminCommands.length > 0" class="grid gap-2">
          <h3 class="type-section-label text-muted-foreground">Admin</h3>
          <div class="flex flex-wrap gap-2">
            <button
              v-for="command in adminCommands"
              :key="command.node"
              type="button"
              class="type-control inline-flex min-h-10 max-w-full items-center gap-2 rounded-md border border-border bg-background px-3 py-2 text-foreground transition-colors hover:bg-muted disabled:opacity-60"
              :disabled="commandStates[command.node]?.state === 'loading'"
              @click="emit('invokeCommand', command)"
            >
              <ShieldCheck class="h-4 w-4 text-primary" aria-hidden="true" />
              <span class="min-w-0 break-words text-left">{{ command.name }}</span>
            </button>
          </div>
        </div>
      </div>

      <p
        v-if="detail || Object.values(commandStates).some((commandState) => commandState.detail)"
        class="type-caption text-muted-foreground"
        role="status"
      >
        {{ detail || Object.values(commandStates).find((commandState) => commandState.detail)?.detail }}
      </p>

      <article
        v-for="command in commands.filter((item) => commandForms[item.node])"
        :key="`form:${command.node}`"
        class="grid gap-3 rounded-lg border border-border bg-muted/30 p-3"
      >
        <div class="min-w-0">
          <h3 class="type-control break-words text-foreground">{{ command.name }}</h3>
          <p v-if="stateDetail(command)" class="type-caption break-words text-muted-foreground">{{ stateDetail(command) }}</p>
        </div>

        <p v-if="blockedReason(command)" class="type-caption text-destructive" role="alert">
          {{ blockedReason(command) }}
        </p>
        <p v-else-if="missingRequiredFields(command).length > 0" class="type-caption text-muted-foreground">
          Complete required fields to continue.
        </p>

        <div class="grid gap-3">
          <template
            v-for="field in visibleFields(command)"
            :key="`${command.node}:${field.name}`"
          >
            <p v-if="field.type === 'fixed'" class="type-caption text-muted-foreground">
              {{ field.value || field.label }}
            </p>

            <label
              v-else-if="field.type === 'boolean'"
              class="type-control flex min-h-10 items-center gap-2 text-foreground"
            >
              <input
                type="checkbox"
                class="h-4 w-4 accent-primary"
                :checked="field.value === '1' || field.value === 'true'"
                :disabled="!!blockedReason(command)"
                @change="setBooleanValueFromEvent(command, field, $event)"
              />
              <span>{{ field.label }}</span>
            </label>

            <label
              v-else-if="field.type === 'list-single'"
              class="type-caption grid gap-1 text-muted-foreground"
            >
              <span>{{ field.label }}<span v-if="field.required" aria-hidden="true"> *</span></span>
              <select
                class="min-h-10 rounded-md border border-border bg-background px-2 text-foreground"
                :value="field.value"
                :required="field.required"
                :disabled="!!blockedReason(command)"
                @change="setSingleValueFromEvent(command, field, $event)"
              >
                <option value="">Select</option>
                <option v-for="option in field.options" :key="option.value" :value="option.value">
                  {{ option.label }}
                </option>
              </select>
              <span v-if="field.description" class="text-muted-foreground/75">{{ field.description }}</span>
            </label>

            <fieldset
              v-else-if="field.type === 'list-multi'"
              class="grid gap-2"
            >
              <legend class="type-caption text-muted-foreground">
                {{ field.label }}<span v-if="field.required"> (required)</span>
              </legend>
              <label
                v-for="option in field.options"
                :key="option.value"
                class="type-control flex min-h-9 items-center gap-2 text-foreground"
              >
                <input
                  type="checkbox"
                  class="h-4 w-4 accent-primary"
                  :checked="field.values.includes(option.value)"
                  :disabled="!!blockedReason(command)"
                  @change="toggleMultiValueFromEvent(command, field, option.value, $event)"
                />
                <span>{{ option.label }}</span>
              </label>
              <p v-if="field.description" class="type-caption text-muted-foreground/75">{{ field.description }}</p>
            </fieldset>

            <label
              v-else-if="field.type === 'text-multi' || field.type === 'jid-multi'"
              class="type-caption grid gap-1 text-muted-foreground"
            >
              <span>{{ field.label }}<span v-if="field.required" aria-hidden="true"> *</span></span>
              <textarea
                class="min-h-24 rounded-md border border-border bg-background px-2 py-2 text-foreground"
                :value="field.values.join('\n')"
                :required="field.required"
                :disabled="!!blockedReason(command)"
                @input="setMultiLineValueFromEvent(command, field, $event)"
              />
              <span v-if="field.description" class="text-muted-foreground/75">{{ field.description }}</span>
            </label>

            <label
              v-else
              class="type-caption grid gap-1 text-muted-foreground"
            >
              <span>{{ field.label }}<span v-if="field.required" aria-hidden="true"> *</span></span>
              <input
                class="min-h-10 rounded-md border border-border bg-background px-2 text-foreground"
                :type="field.type === 'text-private' ? 'password' : 'text'"
                :value="field.value"
                :required="field.required"
                :disabled="field.blocked || !!blockedReason(command)"
                @input="setSingleValueFromEvent(command, field, $event)"
              />
              <span v-if="field.description" class="text-muted-foreground/75">{{ field.description }}</span>
            </label>
          </template>
        </div>

        <div class="flex flex-wrap gap-2">
          <button
            v-for="action in allowedActions(command)"
            :key="`${command.node}:${action}`"
            type="button"
            class="type-control inline-flex min-h-10 items-center gap-2 rounded-md px-3 py-2 disabled:opacity-60"
            :class="action === 'cancel' || action === 'prev' ? 'border border-border bg-background text-foreground hover:bg-muted' : 'bg-primary text-primary-foreground'"
            :disabled="!canSubmitAction(command, action)"
            @click="emit('submitForm', command, action)"
          >
            <ArrowLeft v-if="action === 'prev'" class="h-4 w-4" aria-hidden="true" />
            <Square v-else-if="action === 'cancel'" class="h-4 w-4" aria-hidden="true" />
            <ArrowRight v-else-if="action === 'next'" class="h-4 w-4" aria-hidden="true" />
            <CheckCircle2 v-else class="h-4 w-4" aria-hidden="true" />
            {{ actionLabel(action) }}
          </button>
        </div>
      </article>

      <div
        v-for="command in commands.filter((item) => commandActions[item.node]?.length)"
        :key="`actions:${command.node}`"
        class="flex flex-wrap gap-2"
      >
        <button
          v-for="action in commandActions[command.node]"
          :key="`${command.node}:${action.route}`"
          type="button"
          class="type-control inline-flex min-h-10 max-w-full min-w-0 items-center gap-2 rounded-md border border-border bg-background px-3 py-2 text-foreground hover:bg-muted disabled:opacity-60"
          :disabled="commandStates[command.node]?.state === 'loading'"
          @click="emit('invokeAction', command, action)"
        >
          <span class="min-w-0 break-words text-left">{{ action.label }}</span>
        </button>
      </div>
    </div>
  </section>
</template>
