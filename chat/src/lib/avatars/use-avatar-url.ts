import { computed, type ComputedRef, watch } from "vue";
import { avatarStore } from "./avatar-store";

/**
 * Reactive avatar URL for a JID getter. Retains the JID in the shared
 * store while the calling component is mounted (lazy fetch + periodic
 * revalidation) and releases it on change or unmount.
 */
export function useAvatarUrl(jid: () => string | null | undefined): ComputedRef<string | null> {
  watch(
    jid,
    (value, _previous, onCleanup) => {
      if (!value) return;
      onCleanup(avatarStore.retain(value));
    },
    { immediate: true },
  );
  return computed(() => avatarStore.urlFor(jid()));
}

/**
 * Multi-JID variant for lists rendered without `UserAvatar`: retains every
 * JID the getter yields and returns a reactive reader.
 */
export function useAvatarUrls(
  jids: () => readonly (string | null | undefined)[],
): (jid: string | null | undefined) => string | null {
  watch(
    () => jids().filter((jid): jid is string => !!jid).join("\n"),
    (joined, _previous, onCleanup) => {
      const releases = joined ? joined.split("\n").map((jid) => avatarStore.retain(jid)) : [];
      onCleanup(() => {
        for (const release of releases) release();
      });
    },
    { immediate: true },
  );
  return (jid) => avatarStore.urlFor(jid);
}
