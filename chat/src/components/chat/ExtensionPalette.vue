<script setup lang="ts">
import { computed, ref } from "vue";
import { css } from "styled-system/css";
import { btn, iconbtn, input } from "styled-system/recipes";
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
import { spin } from "./composer-classes";

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

const root = css({ maxHeight: "min(72dvh, 704px)", overflowY: "auto", overscrollBehavior: "contain", bg: "bg.rail", px: "6", py: "3", _narrow: { px: "3" } });
const edgeTop = css({ borderTop: "1px solid {colors.border.soft}" });
const edgeBottom = css({ borderBottom: "1px solid {colors.border.soft}" });
const lane = css({ display: "grid", gap: "3", maxWidth: "read", mx: "auto" });
const head = css({ display: "flex", alignItems: "center", justifyContent: "space-between", gap: "3" });
const minw = css({ minWidth: 0 });
const title = css({ margin: 0, font: "700 16px/1.3 {fonts.display}", color: "fg" });
const muted = css({ margin: 0, textStyle: "caption", color: "fg.muted", overflowWrap: "anywhere" });
const danger = css({ margin: 0, textStyle: "caption", color: "danger.text" });
const inline = css({ display: "inline-flex", alignItems: "center", gap: "2" });
const icon = css({ width: "16px", height: "16px", flex: "none" });
const accent = css({ color: "action.text" });
const success = css({ color: "success.text" });
const dangerFg = css({ color: "danger.text" });
const warningFg = css({ color: "warning.text" });
const stack2 = css({ display: "grid", gap: "2", border: 0, p: 0, m: 0, minWidth: 0 });
const stack3 = css({ display: "grid", gap: "3" });
const eyebrow = css({ margin: 0, textStyle: "eyebrow" });
const wrapRow = css({ display: "flex", flexWrap: "wrap", gap: "2" });
const cmdBtn = css({ maxWidth: "100%", height: "auto", py: "2", _disabled: { opacity: 0.6 } });
const label = css({ minWidth: 0, overflowWrap: "anywhere", textAlign: "left" });
const formCard = css({ display: "grid", gap: "3", p: "4", borderRadius: "panel", border: "1px solid {colors.border.soft}", bg: "surface" });
const formTitle = css({ margin: 0, fontWeight: 700, fontSize: "sm", color: "fg", overflowWrap: "anywhere" });
const check = css({ display: "flex", alignItems: "center", gap: "2", minHeight: "40px", fontSize: "sm", color: "fg" });
const checkbox = css({ width: "16px", height: "16px", accentColor: "action" });
const fieldClass = css({ display: "grid", gap: "1", textStyle: "caption", color: "fg.muted" });
const textarea = css({ height: "auto", minHeight: "96px", py: "2" });
</script>

<template>
  <section
    :class="[root, isTopPinned ? edgeBottom : edgeTop]"
    role="region"
    aria-label="Extensions"
  >
    <div :class="lane">
      <header :class="head">
        <div :class="minw">
          <h2 :class="title">Extensions</h2>
          <p :class="muted">
            Waddle-native actions available in this conversation.
          </p>
        </div>
        <button
          :ref="setCloseButtonRef"
          type="button"
          :class="iconbtn()"
          title="Close extensions"
          aria-label="Close extensions"
          @click="emit('close')"
        >
          <X aria-hidden="true" />
        </button>
      </header>

      <div v-if="state === 'loading'" :class="[muted, inline]" role="status">
        <LoaderCircle :class="[icon, spin]" aria-hidden="true" />
        Loading extensions
      </div>

      <p v-else-if="state === 'error'" :class="danger" role="alert">
        {{ detail || "Could not load extensions." }}
      </p>

      <p v-else-if="commands.length === 0" :class="muted">
        {{ detail || "No extensions are available here." }}
      </p>

      <div v-else :class="stack3">
        <div v-if="memberCommands.length > 0" :class="stack2">
          <h3 :class="eyebrow">Available here</h3>
          <div :class="wrapRow">
            <button
              v-for="command in memberCommands"
              :key="command.node"
              type="button"
              :class="[btn(), cmdBtn]"
              :disabled="commandStates[command.node]?.state === 'loading'"
              @click="emit('invokeCommand', command)"
            >
              <LoaderCircle
                v-if="commandStates[command.node]?.state === 'loading'"
                :class="[icon, spin]"
                aria-hidden="true"
              />
              <CheckCircle2
                v-else-if="commandStates[command.node]?.state === 'success'"
                :class="[icon, success]"
                aria-hidden="true"
              />
              <AlertCircle
                v-else-if="commandStates[command.node]?.state === 'warning' || commandStates[command.node]?.state === 'error'"
                :class="[icon, commandStates[command.node]?.state === 'error' ? dangerFg : warningFg]"
                aria-hidden="true"
              />
              <Play v-else :class="[icon, accent]" aria-hidden="true" />
              <span :class="label">{{ command.name }}</span>
            </button>
          </div>
        </div>

        <div v-if="adminCommands.length > 0" :class="stack2">
          <h3 :class="eyebrow">Admin</h3>
          <div :class="wrapRow">
            <button
              v-for="command in adminCommands"
              :key="command.node"
              type="button"
              :class="[btn(), cmdBtn]"
              :disabled="commandStates[command.node]?.state === 'loading'"
              @click="emit('invokeCommand', command)"
            >
              <ShieldCheck :class="[icon, accent]" aria-hidden="true" />
              <span :class="label">{{ command.name }}</span>
            </button>
          </div>
        </div>
      </div>

      <p
        v-if="detail || Object.values(commandStates).some((commandState) => commandState.detail)"
        :class="muted"
        role="status"
      >
        {{ detail || Object.values(commandStates).find((commandState) => commandState.detail)?.detail }}
      </p>

      <article
        v-for="command in commands.filter((item) => commandForms[item.node])"
        :key="`form:${command.node}`"
        :class="formCard"
      >
        <div :class="minw">
          <h3 :class="formTitle">{{ command.name }}</h3>
          <p v-if="stateDetail(command)" :class="muted">{{ stateDetail(command) }}</p>
        </div>

        <p v-if="blockedReason(command)" :class="danger" role="alert">
          {{ blockedReason(command) }}
        </p>
        <p v-else-if="missingRequiredFields(command).length > 0" :class="muted">
          Complete required fields to continue.
        </p>

        <div :class="stack3">
          <template
            v-for="field in visibleFields(command)"
            :key="`${command.node}:${field.name}`"
          >
            <p v-if="field.type === 'fixed'" :class="muted">
              {{ field.value || field.label }}
            </p>

            <label
              v-else-if="field.type === 'boolean'"
              :class="check"
            >
              <input
                type="checkbox"
                :class="checkbox"
                :checked="field.value === '1' || field.value === 'true'"
                :disabled="!!blockedReason(command)"
                @change="setBooleanValueFromEvent(command, field, $event)"
              />
              <span>{{ field.label }}</span>
            </label>

            <label
              v-else-if="field.type === 'list-single'"
              :class="fieldClass"
            >
              <span>{{ field.label }}<span v-if="field.required" aria-hidden="true"> *</span></span>
              <select
                :class="input()"
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
              <span v-if="field.description" :class="muted">{{ field.description }}</span>
            </label>

            <fieldset
              v-else-if="field.type === 'list-multi'"
              :class="stack2"
            >
              <legend :class="muted">
                {{ field.label }}<span v-if="field.required"> (required)</span>
              </legend>
              <label
                v-for="option in field.options"
                :key="option.value"
                :class="check"
              >
                <input
                  type="checkbox"
                  :class="checkbox"
                  :checked="field.values.includes(option.value)"
                  :disabled="!!blockedReason(command)"
                  @change="toggleMultiValueFromEvent(command, field, option.value, $event)"
                />
                <span>{{ option.label }}</span>
              </label>
              <p v-if="field.description" :class="muted">{{ field.description }}</p>
            </fieldset>

            <label
              v-else-if="field.type === 'text-multi' || field.type === 'jid-multi'"
              :class="fieldClass"
            >
              <span>{{ field.label }}<span v-if="field.required" aria-hidden="true"> *</span></span>
              <textarea
                :class="[input(), textarea]"
                :value="field.values.join('\n')"
                :required="field.required"
                :disabled="!!blockedReason(command)"
                @input="setMultiLineValueFromEvent(command, field, $event)"
              />
              <span v-if="field.description" :class="muted">{{ field.description }}</span>
            </label>

            <label
              v-else
              :class="fieldClass"
            >
              <span>{{ field.label }}<span v-if="field.required" aria-hidden="true"> *</span></span>
              <input
                :class="input()"
                :type="field.type === 'text-private' ? 'password' : 'text'"
                :value="field.value"
                :required="field.required"
                :disabled="field.blocked || !!blockedReason(command)"
                @input="setSingleValueFromEvent(command, field, $event)"
              />
              <span v-if="field.description" :class="muted">{{ field.description }}</span>
            </label>
          </template>
        </div>

        <div :class="wrapRow">
          <button
            v-for="action in allowedActions(command)"
            :key="`${command.node}:${action}`"
            type="button"
            :class="action === 'cancel' || action === 'prev' ? btn() : btn({ variant: 'primary' })"
            :disabled="!canSubmitAction(command, action)"
            @click="emit('submitForm', command, action)"
          >
            <ArrowLeft v-if="action === 'prev'" aria-hidden="true" />
            <Square v-else-if="action === 'cancel'" aria-hidden="true" />
            <ArrowRight v-else-if="action === 'next'" aria-hidden="true" />
            <CheckCircle2 v-else aria-hidden="true" />
            {{ actionLabel(action) }}
          </button>
        </div>
      </article>

      <div
        v-for="command in commands.filter((item) => commandActions[item.node]?.length)"
        :key="`actions:${command.node}`"
        :class="wrapRow"
      >
        <button
          v-for="action in commandActions[command.node]"
          :key="`${command.node}:${action.route}`"
          type="button"
          :class="[btn(), cmdBtn]"
          :disabled="commandStates[command.node]?.state === 'loading'"
          @click="emit('invokeAction', command, action)"
        >
          <span :class="label">{{ action.label }}</span>
        </button>
      </div>
    </div>
  </section>
</template>
