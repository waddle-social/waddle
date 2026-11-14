<script setup lang="ts">
import { onBeforeUnmount } from "vue";
import { Mic, MicOff, PhoneOff, RotateCcw, Video, VideoOff } from "lucide-vue-next";
import { css, cx } from "styled-system/css";
import { huddleCtl } from "styled-system/recipes";
import type { CallTileModel } from "@/lib/calls/call-tiles";
import { TileAttachments, type TileAttachable } from "@/lib/calls/tile-attach";
import CallTile from "./CallTile.vue";

const props = defineProps<{
  tile: CallTileModel | null;
  micEnabled: boolean;
  camEnabled: boolean;
}>();

const emit = defineEmits<{
  toggleMic: [];
  toggleCam: [];
  hangup: [];
  returnToCall: [];
}>();

const attachments = new TileAttachments();

function attach(
  key: string,
  el: HTMLMediaElement | null,
  track: TileAttachable | null,
): void {
  attachments.sync(key, el, track);
}

onBeforeUnmount(() => {
  attachments.detachAll();
});

// Lives in a Document Picture-in-Picture window: the dark stage in
// miniature, one tile over a row of round controls.
const rootClass = css({
  display: "flex",
  flexDirection: "column",
  gap: "2",
  width: "100vw",
  height: "100vh",
  padding: "2",
  boxSizing: "border-box",
  bgColor: "sea.deep",
  color: "fg",
  fontFamily: "body",
});
const tileClass = css({ flex: "1 1 0%", minHeight: 0 });
const emptyClass = css({ display: "grid", placeItems: "center", width: "100%", height: "100%", borderRadius: "12px", border: "1px dashed {colors.border}", color: "fg.muted", fontSize: "sm" });
const controlsClass = css({ display: "flex", justifyContent: "center", gap: "2" });
const buttonClass = cx(huddleCtl(), css({ px: 0, "& svg": { width: "20px", height: "20px" } }));
const dangerClass = css({ bg: "danger", color: "danger.fg", _hover: { bg: "danger.hover" } });
</script>

<template>
  <section :class="rootClass" data-theme="night" aria-label="Call Picture-in-Picture">
    <div :class="tileClass">
      <CallTile
        v-if="tile"
        :key="tile.key"
        :label="tile.label"
        :avatar-jid="tile.identity"
        :attach-key="`pip:${tile.key}`"
        :is-self="tile.isSelf"
        :mirror-video="tile.mirrorVideo"
        :shows-presenting-glyph="tile.showsPresentingGlyph"
        :mic-enabled="tile.micEnabledHint"
        :video-track="tile.videoTrack"
        :attach="attach"
        :interactive="false"
      />
      <div v-else :class="emptyClass">No active video</div>
    </div>
    <!-- No tooltips here: this panel lives in a Document Picture-in-Picture
         window, a separate document a body-portalled tooltip cannot reach.
         The aria-labels carry the names. -->
    <div :class="controlsClass">
      <button
        type="button"
        :class="buttonClass"
        :data-off="!micEnabled"
        :aria-label="micEnabled ? 'Mute' : 'Unmute'"
        @click="emit('toggleMic')"
      >
        <component :is="micEnabled ? Mic : MicOff" aria-hidden="true" />
      </button>
      <button
        type="button"
        :class="buttonClass"
        :aria-label="camEnabled ? 'Camera off' : 'Camera on'"
        @click="emit('toggleCam')"
      >
        <component :is="camEnabled ? Video : VideoOff" aria-hidden="true" />
      </button>
      <button
        type="button"
        :class="buttonClass"
        aria-label="Return to call"
        @click="emit('returnToCall')"
      >
        <RotateCcw aria-hidden="true" />
      </button>
      <button
        type="button"
        :class="[buttonClass, dangerClass]"
        aria-label="Hang up"
        @click="emit('hangup')"
      >
        <PhoneOff aria-hidden="true" />
      </button>
    </div>
  </section>
</template>
