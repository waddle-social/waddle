import { computed, ref, watch, type Ref } from "vue";
import type { WaddleSession } from "@/lib/server-auth";
import type {
  BrowserXmppClient,
  CallInviteInfo,
  IncomingCallInviteEvent,
  MujiCallEvent,
  MujiCallPhase,
} from "@/lib/xmpp-client";

interface PendingInvite {
  fromNick: string;
  invite: CallInviteInfo;
}

function stopStream(stream: MediaStream | null) {
  if (!stream) return;
  for (const track of stream.getTracks()) track.stop();
}

export function useMujiRuntime(
  session: Ref<WaddleSession | null>,
  xmppClient: Ref<BrowserXmppClient | null>,
  activeWaddleId: Ref<string | null>,
  activeChannelId: Ref<string | null>,
  latestCallInvite: Ref<IncomingCallInviteEvent | null>,
  normalizeError: (value: unknown) => string,
) {
  const phase = ref<MujiCallPhase>("idle");
  const sid = ref<string | null>(null);
  const serviceJid = ref<string | null>(null);
  const error = ref("");
  const localStream = ref<MediaStream | null>(null);
  const remoteStream = ref<MediaStream | null>(null);
  const pendingInvite = ref<PendingInvite | null>(null);
  const micEnabled = ref(true);
  const cameraEnabled = ref(true);
  const hasRemoteTracks = ref(false);

  const inCall = computed(() => phase.value !== "idle" && phase.value !== "error");

  function resetCallState() {
    sid.value = null;
    serviceJid.value = null;
    phase.value = "idle";
    hasRemoteTracks.value = false;
    stopStream(localStream.value);
    stopStream(remoteStream.value);
    localStream.value = null;
    remoteStream.value = null;
    micEnabled.value = true;
    cameraEnabled.value = true;
  }

  async function acquireLocalMedia(video: boolean): Promise<MediaStream> {
    if (!navigator.mediaDevices?.getUserMedia) {
      throw new Error("WebRTC media devices are unavailable in this browser.");
    }
    phase.value = "acquiring-media";
    return navigator.mediaDevices.getUserMedia({ audio: true, video });
  }

  function applyMujiEvent(event: MujiCallEvent) {
    if (event.type === "incoming") {
      phase.value = "ringing";
      if (!pendingInvite.value) {
        pendingInvite.value = {
          fromNick: event.peerJid.split("@")[0] ?? "unknown",
          invite: { inviteId: event.sid, muji: true, jingleSid: event.sid, jingleJid: event.peerJid },
        };
      }
      sid.value = event.sid;
      serviceJid.value = event.peerJid;
      return;
    }
    if (event.type === "outgoing") {
      phase.value = "dialing";
      sid.value = event.sid;
      serviceJid.value = event.peerJid;
      return;
    }
    if (event.type === "accepted") {
      phase.value = "active";
      sid.value = event.sid;
      return;
    }
    if (event.type === "connection-state") {
      if (event.state === "connected" || event.state === "completed") phase.value = "active";
      if (event.state === "failed") {
        phase.value = "error";
        error.value = "Call connection failed.";
      }
      return;
    }
    if (event.type === "peer-track-added") {
      sid.value = event.sid;
      phase.value = "active";
      if (!remoteStream.value) remoteStream.value = new MediaStream();
      if (!remoteStream.value.getTracks().some((t) => t.id === event.track.id)) {
        remoteStream.value.addTrack(event.track);
      }
      hasRemoteTracks.value = remoteStream.value.getTracks().length > 0;
      return;
    }
    if (event.type === "peer-track-removed") {
      if (!remoteStream.value) return;
      const existing = remoteStream.value.getTracks().find((t) => t.id === event.track.id);
      if (existing) remoteStream.value.removeTrack(existing);
      hasRemoteTracks.value = remoteStream.value.getTracks().length > 0;
      return;
    }
    if (event.type === "terminated") {
      if (!sid.value || sid.value === event.sid) {
        resetCallState();
      }
      return;
    }
    if (event.type === "error") {
      phase.value = "error";
      error.value = event.detail;
    }
  }

  watch(xmppClient, (client) => {
    if (!client) {
      resetCallState();
      return;
    }
    client.setMujiCallHandler((event) => {
      applyMujiEvent(event);
    });
  });

  watch(latestCallInvite, (event) => {
    if (!event) return;
    pendingInvite.value = {
      fromNick: event.nick,
      invite: event.invite,
    };
  });

  async function startCall(video: boolean) {
    const client = xmppClient.value;
    const waddleId = activeWaddleId.value;
    const channelId = activeChannelId.value;
    if (!client || !waddleId || !channelId) return;
    error.value = "";
    try {
      const stream = await acquireLocalMedia(video);
      localStream.value = stream;
      const result = await client.startMujiCall(waddleId, channelId, stream, { video });
      if (!result) throw new Error("Failed to start Muji session.");
      sid.value = result.sid;
      serviceJid.value = result.serviceJid;
      pendingInvite.value = null;
      phase.value = "dialing";
    } catch (err) {
      phase.value = "error";
      error.value = normalizeError(err);
      stopStream(localStream.value);
      localStream.value = null;
    }
  }

  async function joinPendingInvite(video = true) {
    const client = xmppClient.value;
    const waddleId = activeWaddleId.value;
    const channelId = activeChannelId.value;
    const invite = pendingInvite.value?.invite;
    if (!client || !waddleId || !channelId || !invite) return;
    error.value = "";
    try {
      const stream = await acquireLocalMedia(video);
      localStream.value = stream;
      const joinOpts: { sid?: string; jingleJid?: string; video?: boolean } = { video };
      if (invite.jingleSid) joinOpts.sid = invite.jingleSid;
      if (invite.jingleJid) joinOpts.jingleJid = invite.jingleJid;
      const result = await client.joinMujiCall(waddleId, channelId, stream, joinOpts);
      if (!result) throw new Error("Failed to join Muji session.");
      sid.value = result.sid;
      serviceJid.value = result.serviceJid;
      pendingInvite.value = null;
      phase.value = "dialing";
    } catch (err) {
      phase.value = "error";
      error.value = normalizeError(err);
      stopStream(localStream.value);
      localStream.value = null;
    }
  }

  function dismissInvite() {
    pendingInvite.value = null;
    if (phase.value === "ringing") {
      phase.value = sid.value ? "dialing" : "idle";
    }
  }

  function toggleMicrophone() {
    if (!localStream.value) return;
    micEnabled.value = !micEnabled.value;
    for (const track of localStream.value.getAudioTracks()) {
      track.enabled = micEnabled.value;
    }
  }

  function toggleCamera() {
    if (!localStream.value) return;
    cameraEnabled.value = !cameraEnabled.value;
    for (const track of localStream.value.getVideoTracks()) {
      track.enabled = cameraEnabled.value;
    }
  }

  function endCall() {
    phase.value = "ending";
    xmppClient.value?.endMujiCall(sid.value ?? undefined);
    resetCallState();
    pendingInvite.value = null;
    error.value = "";
  }

  watch([activeWaddleId, activeChannelId], () => {
    pendingInvite.value = null;
    if (!inCall.value) return;
    endCall();
  });

  watch(session, (value) => {
    if (!value) {
      endCall();
    }
  });

  return {
    phase,
    sid,
    serviceJid,
    error,
    inCall,
    localStream,
    remoteStream,
    pendingInvite,
    hasRemoteTracks,
    micEnabled,
    cameraEnabled,
    startAudioCall: () => startCall(false),
    startVideoCall: () => startCall(true),
    joinPendingInvite,
    dismissInvite,
    toggleMicrophone,
    toggleCamera,
    endCall,
  };
}
