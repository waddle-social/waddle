<script setup lang="ts">
import { computed } from "vue";
import { ExternalLink } from "lucide-vue-next";
import { calendarFeedSubscriptionHref } from "@/lib/calendar-feed-url";

const props = defineProps<{
  url: string;
}>();

const subscriptionHref = computed(() => calendarFeedSubscriptionHref(props.url));

function selectFeedUrl(event: FocusEvent) {
  if (event.target instanceof HTMLInputElement) {
    event.target.select();
  }
}
</script>

<template>
  <div class="flex flex-wrap items-center gap-2 rounded-md border border-border bg-muted/30 px-3 py-2">
    <span class="text-xs font-medium text-muted-foreground">
      Calendar subscription URL
    </span>
    <a
      v-if="subscriptionHref"
      :href="subscriptionHref"
      target="_blank"
      rel="noopener noreferrer"
      class="inline-flex min-h-11 items-center gap-1 rounded-md border border-input px-2.5 py-1.5 text-xs font-medium text-primary hover:bg-background hover:underline"
    >
      <ExternalLink class="h-3.5 w-3.5" aria-hidden="true" />
      Subscribe to calendar
    </a>
    <input
      class="min-h-11 min-w-0 basis-full rounded-md border border-input bg-background px-2 py-1.5 text-xs text-foreground sm:basis-0 sm:flex-1"
      readonly
      :value="url"
      aria-label="Calendar feed URL"
      @focus="selectFeedUrl"
    />
  </div>
</template>
