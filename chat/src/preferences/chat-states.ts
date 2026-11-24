import { computed, ref } from "vue";
import {
  chatStatesAreEnabled,
  readStoredChatStatePreference,
  type ChatStatePreference,
  writeStoredChatStatePreference,
} from "@/lib/chat-state-preference";

const mode = ref<ChatStatePreference>(readStoredChatStatePreference());

function setChatStatePreference(value: ChatStatePreference) {
  mode.value = value;
  writeStoredChatStatePreference(value);
}

export function useChatStatePreference() {
  return {
    mode,
    setChatStatePreference,
    sendChatStates: computed(() => chatStatesAreEnabled(mode.value)),
  };
}
