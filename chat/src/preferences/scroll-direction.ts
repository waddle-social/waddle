import { computed, ref } from "vue";
import {
  isTopPinnedScrollDirection,
  readStoredScrollDirection,
  type ScrollDirectionMode,
  writeStoredScrollDirection,
} from "@/lib/scroll-direction";

const mode = ref<ScrollDirectionMode>(readStoredScrollDirection());

function setScrollDirection(value: ScrollDirectionMode) {
  mode.value = value;
  writeStoredScrollDirection(value);
}

export function useScrollDirectionPreference() {
  return {
    mode,
    setScrollDirection,
    isTopPinned: computed(() => isTopPinnedScrollDirection(mode.value)),
  };
}
