import { computed, ref } from "vue";
import {
  readStoredConsistentColorPreference,
  type ConsistentColorPreference,
  writeStoredConsistentColorPreference,
} from "@/lib/consistent-color-preference";

const mode = ref<ConsistentColorPreference>(readStoredConsistentColorPreference());

function setConsistentColorPreference(value: ConsistentColorPreference) {
  mode.value = value;
  writeStoredConsistentColorPreference(value);
}

export function useConsistentColorPreference() {
  return {
    mode,
    setConsistentColorPreference,
    colorizeIdentities: computed(() => mode.value === "on"),
  };
}
