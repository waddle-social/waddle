<script setup lang="ts" generic="T extends string">
import { computed } from "vue";
import { Select, createListCollection } from "@ark-ui/vue/select";
import type { SelectValueChangeDetails } from "@ark-ui/vue/select";
import { Check, ChevronDown } from "lucide-vue-next";
import { css, cx } from "styled-system/css";
import { menuClasses } from "@/ui/menu-classes";

interface AppSelectOption<V extends string> {
  value: V;
  label: string;
  disabled?: boolean;
}

/**
 * Non-native select on Ark Select, sharing the `AppMenu` surface.
 * `class` and other attrs (aria-label, aria-invalid) land on the trigger,
 * so call sites keep their `input()`-based sizing. `id` becomes the
 * trigger id so an existing `<label for>` still names it. An empty
 * `modelValue` shows `placeholder` unless an option has value "".
 */
defineOptions({ inheritAttrs: false });

const props = withDefaults(
  defineProps<{
    modelValue: T | "";
    options: readonly AppSelectOption<T>[];
    placeholder?: string;
    disabled?: boolean;
    required?: boolean;
    id?: string;
  }>(),
  { placeholder: "", disabled: false, required: false, id: undefined },
);

const emit = defineEmits<{ "update:modelValue": [value: T] }>();

const collection = computed(() =>
  createListCollection({ items: props.options.map((o) => ({ ...o })), isItemDisabled: (o) => !!o.disabled }),
);
const value = computed(() =>
  props.modelValue === "" && !props.options.some((o) => o.value === "") ? [] : [props.modelValue],
);
const ids = computed(() => (props.id ? { trigger: props.id } : undefined));

function onValueChange(details: SelectValueChangeDetails<AppSelectOption<T>>) {
  const next = details.value[0] as T | undefined;
  if (next !== undefined && next !== props.modelValue) emit("update:modelValue", next);
}

const triggerClass = css({
  display: "inline-flex",
  alignItems: "center",
  justifyContent: "space-between",
  gap: "2",
  textAlign: "left",
  cursor: "pointer",
  "& [data-part=value-text]": { flex: 1, minWidth: 0, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" },
  "& [data-part=value-text][data-placeholder-shown]": { color: "fg.soft" },
  _disabled: { cursor: "not-allowed", opacity: 0.6 },
});
const indicatorClass = css({
  display: "inline-flex",
  flex: "none",
  color: "fg.muted",
  transition: "transform {durations.fast} {easings.out}",
  "& svg": { width: "16px", height: "16px" },
  _open: { transform: "rotate(180deg)" },
});
const contentClass = css({
  minWidth: "var(--reference-width)",
  maxHeight: "min(320px, var(--available-height))",
  overflowY: "auto",
  // Selects live in dialogs and drawers, so lift the list above them
  // (menuClasses.content uses `dropdown`, which sits below `dialog`).
  "&[data-part=content]": { zIndex: "calc({zIndex.dialog} + 1)" },
});
const itemIndicatorClass = css({ marginLeft: "auto", display: "inline-flex", color: "fg" });
</script>

<template>
  <Select.Root
    :collection="collection"
    :model-value="value"
    :disabled="disabled"
    :required="required"
    :ids="ids"
    :positioning="{ gutter: 6, sameWidth: false }"
    lazy-mount
    unmount-on-exit
    @value-change="onValueChange"
  >
    <Select.Control>
      <Select.Trigger v-bind="$attrs" :class="triggerClass">
        <Select.ValueText :placeholder="placeholder" />
        <Select.Indicator :class="indicatorClass"><ChevronDown aria-hidden="true" /></Select.Indicator>
      </Select.Trigger>
    </Select.Control>
    <Teleport to="body">
      <Select.Positioner>
        <Select.Content :class="cx(menuClasses.content, contentClass)">
          <Select.Item v-for="option in collection.items" :key="option.value" :item="option" :class="menuClasses.item">
            <Select.ItemText>{{ option.label }}</Select.ItemText>
            <Select.ItemIndicator :class="itemIndicatorClass"><Check aria-hidden="true" /></Select.ItemIndicator>
          </Select.Item>
        </Select.Content>
      </Select.Positioner>
    </Teleport>
    <Select.HiddenSelect />
  </Select.Root>
</template>
