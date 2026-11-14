// Dev-only call fixtures for /_preview (see PreviewApp.vue). They write the
// real call stores so the real call surfaces render; nothing connects.
//   huddle-live      someone else's huddle in #design (join card, live now)
//   huddle-busy      you are in #releases, #design has another huddle
//   huddle-room      you are in the #design huddle (split, right panel)
//   huddle-expanded  same call, expanded
//   huddle-minimized in the #design huddle, looking at #general
//   call-incoming | call-outgoing | call-active | call-ended | call-error  (DM with linus)
// Query: &state=connecting|waiting  &notice=mic|audio|sharing|poor|reconnecting|pip
//        &view=gallery|speaker  &dock=participants|chat
import { $callState, $lastCallError } from "@/lib/calls/call-store";
import { $callConnecting } from "@/lib/calls/call-controls";
import { $callMicEnabled } from "@/lib/calls/call-mic-state";
import { $callCamEnabled } from "@/lib/calls/call-cam-state";
import { $callScreenShareEnabled, $callScreenShareSupported } from "@/lib/calls/screen-share-state";
import { $callUiMode } from "@/lib/calls/ui-mode";
import { $callViewMode } from "@/lib/calls/call-view-state";
import { $callDockOpen, $callDockTab } from "@/lib/calls/call-dock-state";
import { $mucCallMedia, $mucCallParticipants } from "@/lib/calls/muc-call-presence";
import { $mucCallLiveParticipants } from "@/lib/calls/muc-call-live-participants";
import { $mucRaisedHands } from "@/lib/calls/call-raised-hand";
import { $mucMutedParticipants } from "@/lib/calls/call-mute";
import { $callMediaIssues } from "@/lib/calls/call-media-issues";
import { setCallAudioPlaybackBlocked } from "@/lib/calls/call-audio-playback";
import { setCallConnectionPhase, setCallConnectionQuality } from "@/lib/calls/connection-quality";
import { $callPictureInPictureActive, $callPictureInPictureMode, $callPictureInPictureSupport } from "@/lib/calls/call-picture-in-picture";
import { useCallEngine } from "@/lib/calls/use-call-engine";
import type { RemoteMediaTrack } from "@/lib/calls/engine";
import type { ChatAppController } from "@/shell/chat-app-controller";

const q = new URLSearchParams(location.search);
const room = (name: string) => `${name}@muc.waddle.test`;
const full = (user: string) => `${user}@waddle.test/web`;
const SELF = full("ada");
const join = { url: "wss://livekit.waddle.test", room: "r", identity: SELF, token: "preview" };
const media = { audio: true, video: true };
const OTHERS = ["linus", "grace", "ken", "mika"];

// A track stub is enough: tiles fall back to avatars without video, and the
// audio sink only attaches/detaches.
const stub = { attach: () => document.createElement("audio"), detach: () => [] } as unknown as RemoteMediaTrack["track"];
const audioTrack = (user: string): RemoteMediaTrack => ({
  participantIdentity: full(user), publicationSid: `pa-${user}`, kind: "audio", source: "microphone", track: stub,
});

function joinMuc(roomName: string, others: string[]) {
  const jid = room(roomName);
  $mucCallParticipants.setKey(jid, ["ada", ...others]);
  $mucCallMedia.setKey(jid, media);
  $mucCallLiveParticipants.setKey(jid, others.map(full));
  $callState.set({ phase: "active", peer: jid, sid: `pv-${roomName}`, media, join, kind: "muc", selfNick: "ada", selfFullJid: SELF });
  const { remoteTracks, activeSpeakerIdentities } = useCallEngine();
  remoteTracks.value = others.map(audioTrack);
  if (others[0]) activeSpeakerIdentities.value = new Set([full(others[0])]);
  $mucRaisedHands.setKey(jid, others[1] ? [full(others[1])] : []);
  $mucMutedParticipants.setKey(jid, others[2] ? [full(others[2])] : []);
}

function applyCallOptions() {
  const state = q.get("state");
  if (state === "connecting") $callConnecting.set(true);
  setCallConnectionPhase(state === "connecting" ? "disconnected" : "connected");
  setCallConnectionQuality("good");
  $callMicEnabled.set(true);
  $callCamEnabled.set(false);
  $callScreenShareSupported.set(true);
  $callPictureInPictureSupport.set("document");
  const notice = q.get("notice");
  if (notice === "mic") {
    $callMicEnabled.set(false);
    $callMediaIssues.set({ mic: "denied", cam: null, screen: null });
  }
  if (notice === "audio") setCallAudioPlaybackBlocked(true);
  if (notice === "sharing") $callScreenShareEnabled.set(true);
  if (notice === "poor") setCallConnectionQuality("poor");
  if (notice === "reconnecting") setCallConnectionPhase("reconnecting");
  if (notice === "pip") {
    $callPictureInPictureActive.set(true);
    $callPictureInPictureMode.set("document");
  }
  if (q.get("view") === "speaker") $callViewMode.set("speaker");
  const dock = q.get("dock");
  if (dock === "participants" || dock === "chat") {
    $callDockTab.set(dock);
    $callDockOpen.set(true);
  }
}

export function seedCallPreview(c: ChatAppController, screen: string) {
  const waiting = q.get("state") === "waiting";
  const others = waiting ? [] : OTHERS;
  switch (screen) {
    case "huddle-live":
      $mucCallParticipants.setKey(room("design"), ["linus", "grace", "ken"]);
      $mucCallMedia.setKey(room("design"), { audio: true, video: false });
      void c.selectChannel("design");
      break;
    case "huddle-busy":
      $mucCallParticipants.setKey(room("design"), ["linus", "grace"]);
      $mucCallMedia.setKey(room("design"), { audio: true, video: false });
      joinMuc("releases", ["mika"]);
      $callUiMode.set("split");
      void c.selectChannel("design");
      break;
    case "huddle-room":
    case "huddle-expanded":
    case "huddle-minimized":
      joinMuc("design", others);
      $callUiMode.set(screen === "huddle-expanded" ? "expanded" : "split");
      void c.selectChannel(screen === "huddle-minimized" ? "general" : "design");
      break;
    case "call-incoming":
      $callState.set({ phase: "incoming", from: full("linus"), sid: "pv-in", media });
      void c.selectDm("linus@waddle.test");
      break;
    case "call-outgoing":
      $callState.set({ phase: "outgoing", to: "linus@waddle.test", sid: "pv-out", media: { audio: true, video: false }, ringing: true });
      void c.selectDm("linus@waddle.test");
      break;
    case "call-active":
      $callState.set({ phase: "active", peer: full("linus"), sid: "pv-dm", media, join, kind: "dm" });
      useCallEngine().remoteTracks.value = [audioTrack("linus")];
      $callUiMode.set(q.get("mode") === "expanded" ? "expanded" : "split");
      void c.selectDm("linus@waddle.test");
      break;
    case "call-ended":
      $callState.set({ phase: "ended", sid: "pv-end", reason: "decline" });
      void c.selectDm("linus@waddle.test");
      break;
    case "call-error":
      $lastCallError.set("Couldn't reach linus's device. Check your connection and try again.");
      void c.selectDm("linus@waddle.test");
      break;
  }
  applyCallOptions();
}
