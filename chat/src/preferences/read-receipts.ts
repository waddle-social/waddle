import { computed, ref } from "vue";
import {
  readReceiptsAreEnabled,
  readStoredReadReceiptPreference,
  type ReadReceiptPreference,
  writeStoredReadReceiptPreference,
} from "@/lib/read-receipt-preference";

const mode = ref<ReadReceiptPreference>(readStoredReadReceiptPreference());

function setReadReceiptPreference(value: ReadReceiptPreference) {
  mode.value = value;
  writeStoredReadReceiptPreference(value);
}

export function useReadReceiptPreference() {
  return {
    mode,
    setReadReceiptPreference,
    sendDisplayedMarkers: computed(() => readReceiptsAreEnabled(mode.value)),
  };
}
