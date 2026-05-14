import { ref, type Ref } from "vue";
import { CallEngine, type RemoteMediaTrack } from "./engine";

/**
 * Process-wide singleton: only one call engine should ever exist
 * because the call-store only tracks one call at a time. The Vue
 * tree may mount/unmount the overlay component repeatedly during a
 * call; tying the engine to the component lifecycle would tear it
 * down on every re-render, so the engine outlives the component.
 */
let singletonEngine: CallEngine | null = null;
const remoteTracks: Ref<RemoteMediaTrack[]> = ref([]);

export function useCallEngine(): {
  engine: CallEngine;
  remoteTracks: Ref<RemoteMediaTrack[]>;
} {
  if (!singletonEngine) {
    singletonEngine = new CallEngine();
    singletonEngine.on("trackSubscribed", (track) => {
      remoteTracks.value = [...remoteTracks.value, track];
    });
    singletonEngine.on("trackUnsubscribed", (track) => {
      remoteTracks.value = remoteTracks.value.filter(
        (existing) => existing.publicationSid !== track.publicationSid,
      );
    });
    singletonEngine.on("disconnected", () => {
      remoteTracks.value = [];
    });
  }
  return { engine: singletonEngine, remoteTracks };
}
