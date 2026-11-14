<script setup lang="ts">
import { computed } from "vue";
import { ExternalLink } from "lucide-vue-next";
import { css, cx } from "styled-system/css";
import { btn, input, kicker } from "styled-system/recipes";
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

const rootClass = css({
  display: "flex",
  flexDirection: "column",
  gap: "2.5",
  padding: "14px 16px",
  borderRadius: "12px",
  bg: "surface",
  border: "1px solid {colors.border.soft}",
});
const headClass = css({ display: "flex", alignItems: "center", gap: "2.5", flexWrap: "wrap" });
const titleClass = cx(kicker(), css({ margin: 0, flex: 1 }));
const linkClass = cx(btn({ size: "sm" }), css({ textDecoration: "none", _coarse: { minHeight: "44px" } }));
const inputClass = cx(input(), css({ width: "100%", minWidth: 0, fontFamily: "mono", fontSize: "13px" }));
</script>

<template>
  <section :class="rootClass" aria-label="Calendar subscription URL">
    <div :class="headClass">
      <h2 :class="titleClass">Subscribe in your calendar app</h2>
      <a
        v-if="subscriptionHref"
        :href="subscriptionHref"
        target="_blank"
        rel="noopener noreferrer"
        :class="linkClass"
      >
        Subscribe to calendar
        <ExternalLink aria-hidden="true" />
      </a>
    </div>
    <input
      :class="inputClass"
      readonly
      :value="url"
      aria-label="Calendar feed URL"
      @focus="selectFeedUrl"
    />
  </section>
</template>
