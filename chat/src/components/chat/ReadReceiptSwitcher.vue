<script setup lang="ts">
import { useReadReceiptPreference } from "@/preferences/read-receipts";

const { mode, setReadReceiptPreference } = useReadReceiptPreference();

function onToggle(event: Event) {
  const checked = event.target instanceof HTMLInputElement
    ? event.target.checked
    : false;
  setReadReceiptPreference(checked ? "send" : "suppress");
}
</script>

<template>
  <label class="flex items-start justify-between gap-4 rounded-lg border border-border bg-muted/30 px-3 py-3">
    <span class="grid gap-1">
      <span class="type-control text-foreground">Send read receipts</span>
      <span class="type-meta text-muted-foreground">
        Share XEP-0333 displayed markers with message senders.
      </span>
    </span>
    <input
      class="mt-1 h-4 w-4 accent-primary"
      type="checkbox"
      :checked="mode === 'send'"
      @change="onToggle"
    />
  </label>
</template>
