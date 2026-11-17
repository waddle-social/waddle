<script setup lang="ts">
import { Dialog } from "@ark-ui/vue/dialog";
import { X } from "lucide-vue-next";
import { btn, iconbtn } from "styled-system/recipes";
import AppDialog from "@/components/ui/AppDialog.vue";
import { dialogParts } from "@/ui/part-classes";

/**
 * Confirm / destructive action prompt. An `alertdialog` on `AppDialog`:
 * Ark links the title and message as the dialog's accessible name and
 * description.
 */
const open = defineModel<boolean>("open", { required: true });

defineProps<{
  title: string;
  message: string;
  confirmLabel?: string;
  destructive?: boolean;
  loading?: boolean;
}>();

const emit = defineEmits<{
  confirm: [];
}>();
</script>

<template>
  <AppDialog v-model:open="open" role="alertdialog" size="sm">
    <div :class="dialogParts.head">
      <Dialog.Title as-child>
        <h2>{{ title }}</h2>
      </Dialog.Title>
      <Dialog.CloseTrigger as-child>
        <button :class="iconbtn({ size: 'sm' })" type="button" aria-label="Close confirmation dialog">
          <X aria-hidden="true" />
        </button>
      </Dialog.CloseTrigger>
    </div>

    <div :class="dialogParts.body">
      <Dialog.Description as-child>
        <p>{{ message }}</p>
      </Dialog.Description>
    </div>

    <div :class="dialogParts.foot">
      <Dialog.CloseTrigger as-child>
        <button :class="btn()" type="button">Cancel</button>
      </Dialog.CloseTrigger>
      <button
        :class="btn({ variant: destructive ? 'danger' : 'primary' })"
        type="button"
        :disabled="loading"
        @click="emit('confirm')"
      >
        {{ loading ? "Deleting…" : (confirmLabel ?? "Confirm") }}
      </button>
    </div>
  </AppDialog>
</template>
