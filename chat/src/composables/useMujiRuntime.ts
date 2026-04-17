import { computed, ref, watch, type Ref } from "vue";
import { Room, RoomEvent, Track } from "livekit-client";
import type { WaddleSession } from "@/lib/server-auth";
import type {
  BrowserXmppClient,
  CallInviteInfo,
  IncomingCallInviteEvent,
  MujiCallPhase,
  RemoteParticipant,
  XmppStatusSnapshot,
} from "@/lib/xmpp-client";

interface PendingInvite {
  roomJid?: string;
  fromNick: string;
  invite: CallInviteInfo;
}

interface RecoverableCall {
  inviteId: string;
  waddleId: string;
  channelId: string;
  video: boolean;
}

function stopStream(stream: MediaStream | null) {
  if (!stream) return;
  for (const track of stream.getTracks()) track.stop();
}

const CALL_SETUP_UNAVAILABLE_CONDITIONS = new Set([
  "item-not-found",
  "feature-not-implemented",
  "service-unavailable",
  "remote-server-timeout",
]);

function xmppErrorCondition(value: unknown): string | null {
  if (value && typeof value === "object") {
    const candidate = value as Record<string, unknown>;
    if (typeof candidate.condition === "string" && candidate.condition.length > 0) {
      return candidate.condition;
    }
    if (candidate.error && typeof candidate.error === "object") {
      const nested = candidate.error as Record<string, unknown>;
      if (typeof nested.condition === "string" && nested.condition.length > 0) {
        return nested.condition;
      }
    }
  }

  if (value instanceof Error) {
    const message = value.message.toLowerCase();
    if (message.includes("object can not be found here") || message.includes("item-not-found")) {
      return "item-not-found";
    }
    if (message.includes("feature requested is not implemented")) {
      return "feature-not-implemented";
    }
  }

  return null;
}

function normalizeCallError(
  value: unknown,
  normalizeError: (value: unknown) => string,
): string {
  if (value instanceof DOMException) {
    switch (value.name) {
      case "NotFoundError":
        return "No microphone found. Please connect a microphone and try again.";
      case "NotAllowedError":
        return "Microphone access was denied. Please allow microphone access in your browser settings.";
      case "NotReadableError":
        return "Your microphone is in use by another application.";
    }
  }
  const condition = xmppErrorCondition(value);
  if (condition && CALL_SETUP_UNAVAILABLE_CONDITIONS.has(condition)) {
    return "Calling is unavailable on this server right now.";
  }
  return normalizeError(value);
}

export function useMujiRuntime(
  session: Ref<WaddleSession | null>,
  xmppClient: Ref<BrowserXmppClient | null>,
  xmppStatus: Ref<XmppStatusSnapshot>,
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
  const remoteParticipants = ref<Map<string, RemoteParticipant>>(new Map());
  const pendingInvite = ref<PendingInvite | null>(null);
  const micEnabled = ref(true);
  const cameraEnabled = ref(true);
  const hasRemoteTracks = ref(false);
  const recoverableCall = ref<RecoverableCall | null>(null);
  let leavePromise: Promise<void> | null = null;
  let rejoinPromise: Promise<void> | null = null;
  let room: Room | null = null;
  let roomToken: symbol | null = null;
  let roomDisconnecting = false;

  const inCall = computed(() => phase.value !== "idle" && phase.value !== "error");
  const isSwitching = computed(() => phase.value === "switching");

  function clearRemoteMediaState() {
    sid.value = null;
    serviceJid.value = null;
    hasRemoteTracks.value = false;
    remoteParticipants.value = new Map();
  }

  function resetCallState() {
    clearRemoteMediaState();
    phase.value = "idle";
    stopStream(localStream.value);
    localStream.value = null;
    micEnabled.value = true;
    cameraEnabled.value = true;
    recoverableCall.value = null;
  }

  function syncRemoteParticipants(activeRoom: Room) {
    const next = new Map<string, RemoteParticipant>();

    for (const participant of activeRoom.remoteParticipants.values()) {
      const tracks = Array.from(participant.trackPublications.values())
        .flatMap((publication) => (publication.track ? [publication.track.mediaStreamTrack] : []))
        .filter((track) => track.readyState === "live");
      if (tracks.length === 0) continue;

      next.set(participant.identity, {
        jid: participant.name || participant.identity,
        stream: new MediaStream(tracks),
      });
    }

    remoteParticipants.value = next;
    hasRemoteTracks.value = next.size > 0;
  }

  function applyConnectedPhase(fallback: MujiCallPhase = "dialing") {
    if (phase.value === "ending" || phase.value === "switching") return;
    phase.value = hasRemoteTracks.value ? "active" : fallback;
  }

  function trackRecoverableCall(inviteId: string, video: boolean) {
    const waddleId = activeWaddleId.value;
    const channelId = activeChannelId.value;
    if (!waddleId || !channelId) return;
    recoverableCall.value = {
      inviteId,
      waddleId,
      channelId,
      video,
    };
  }

  async function acquireLocalMedia(video: boolean): Promise<MediaStream> {
    if (!navigator.mediaDevices?.getUserMedia) {
      throw new Error("WebRTC media devices are unavailable in this browser.");
    }

    phase.value = "acquiring-media";
    if (video) {
      try {
        return await navigator.mediaDevices.getUserMedia({ audio: true, video: true });
      } catch (err) {
        if (err instanceof DOMException && (err.name === "NotFoundError" || err.name === "OverconstrainedError")) {
          cameraEnabled.value = false;
          return navigator.mediaDevices.getUserMedia({ audio: true, video: false });
        }
        throw err;
      }
    }

    cameraEnabled.value = false;
    return navigator.mediaDevices.getUserMedia({ audio: true, video: false });
  }

  async function disconnectRoom() {
    const activeRoom = room;
    room = null;
    roomToken = null;
    clearRemoteMediaState();
    if (!activeRoom) return;

    roomDisconnecting = true;
    try {
      await activeRoom.disconnect();
    } finally {
      roomDisconnecting = false;
    }
  }

  async function publishLocalTracks(activeRoom: Room, stream: MediaStream) {
    const audioTrack = stream.getAudioTracks()[0];
    if (!audioTrack) {
      throw new Error("No microphone track is available for this call.");
    }

    await activeRoom.localParticipant.publishTrack(audioTrack, {
      source: Track.Source.Microphone,
    });

    const videoTrack = stream.getVideoTracks()[0];
    if (videoTrack) {
      await activeRoom.localParticipant.publishTrack(videoTrack, {
        source: Track.Source.Camera,
      });
    }

    micEnabled.value = audioTrack.enabled;
    cameraEnabled.value = !!videoTrack && videoTrack.enabled;
  }

  function bindRoomEvents(activeRoom: Room, token: symbol) {
    const ifCurrent = (handler: () => void) => {
      if (room !== activeRoom || roomToken !== token) return;
      handler();
    };

    activeRoom.on(RoomEvent.ParticipantConnected, () => {
      ifCurrent(() => {
        syncRemoteParticipants(activeRoom);
        applyConnectedPhase();
      });
    });

    activeRoom.on(RoomEvent.ParticipantDisconnected, () => {
      ifCurrent(() => {
        syncRemoteParticipants(activeRoom);
        applyConnectedPhase();
      });
    });

    activeRoom.on(RoomEvent.TrackSubscribed, () => {
      ifCurrent(() => {
        syncRemoteParticipants(activeRoom);
        applyConnectedPhase("active");
      });
    });

    activeRoom.on(RoomEvent.TrackUnsubscribed, () => {
      ifCurrent(() => {
        syncRemoteParticipants(activeRoom);
        applyConnectedPhase();
      });
    });

    activeRoom.on(RoomEvent.Reconnecting, () => {
      ifCurrent(() => {
        phase.value = "reconnecting";
      });
    });

    activeRoom.on(RoomEvent.Reconnected, () => {
      ifCurrent(() => {
        syncRemoteParticipants(activeRoom);
        applyConnectedPhase();
      });
    });

    activeRoom.on(RoomEvent.Disconnected, () => {
      if (!room || room !== activeRoom || roomToken !== token) return;

      room = null;
      roomToken = null;
      clearRemoteMediaState();

      if (roomDisconnecting) return;
      if (!recoverableCall.value) {
        phase.value = "idle";
        return;
      }

      phase.value = "reconnecting";
      if (xmppStatus.value.state === "online") {
        void attemptRejoin();
      }
    });
  }

  async function connectToMediaRoom(
    client: BrowserXmppClient,
    waddleId: string,
    channelId: string,
    stream: MediaStream,
    video: boolean,
  ) {
    const sessionInfo = await client.requestChannelMediaJoin(waddleId, channelId, { video });
    const nextRoom = new Room();
    const nextToken = Symbol("livekit-room");

    room = nextRoom;
    roomToken = nextToken;
    serviceJid.value = sessionInfo.roomName;
    bindRoomEvents(nextRoom, nextToken);

    try {
      await nextRoom.connect(sessionInfo.serverUrl, sessionInfo.token);
      await publishLocalTracks(nextRoom, stream);
      syncRemoteParticipants(nextRoom);
      applyConnectedPhase();
      return sessionInfo;
    } catch (err) {
      if (room === nextRoom && roomToken === nextToken) {
        await disconnectRoom();
      }
      throw err;
    }
  }

  async function leaveCurrentCall(opts: { clearPendingInvite?: boolean } = {}) {
    if (leavePromise) return leavePromise;

    const activeInviteId = sid.value ?? recoverableCall.value?.inviteId;
    const context = recoverableCall.value;
    const activeRoom = room;
    if (!activeInviteId && !context && !activeRoom) {
      resetCallState();
      return;
    }

    const { clearPendingInvite = true } = opts;
    leavePromise = (async () => {
      phase.value = "ending";
      try {
        const client = xmppClient.value;
        if (client && context) {
          try {
            await client.sendCallLeft(context.waddleId, context.channelId, activeInviteId);
          } catch {
            // best-effort leave signal
          }
        }
        await disconnectRoom();
      } finally {
        resetCallState();
        if (clearPendingInvite) pendingInvite.value = null;
        error.value = "";
      }
    })();

    try {
      await leavePromise;
    } finally {
      leavePromise = null;
    }
  }

  async function ensureNotInCall(action: string): Promise<boolean> {
    if (!inCall.value) return true;
    error.value = `You're already in a call. End or switch calls to ${action}.`;
    return false;
  }

  async function startCall(video: boolean) {
    const client = xmppClient.value;
    const waddleId = activeWaddleId.value;
    const channelId = activeChannelId.value;
    if (!client || !waddleId || !channelId) return;
    if (!(await ensureNotInCall("start another one"))) return;

    error.value = "";
    try {
      const stream = await acquireLocalMedia(video);
      const actualVideo = stream.getVideoTracks().length > 0;
      localStream.value = stream;

      const sessionInfo = await connectToMediaRoom(
        client,
        waddleId,
        channelId,
        stream,
        actualVideo,
      );

      const inviteId = await client.sendCallInvite(waddleId, channelId, {
        externalUri: undefined,
        video: actualVideo,
        muji: true,
      });
      if (!inviteId) {
        throw new Error("Failed to send the call invite.");
      }

      sid.value = inviteId;
      serviceJid.value = sessionInfo.roomName;
      trackRecoverableCall(inviteId, actualVideo);
      pendingInvite.value = null;
      applyConnectedPhase("dialing");
    } catch (err) {
      await disconnectRoom();
      phase.value = "error";
      error.value = normalizeCallError(err, normalizeError);
      stopStream(localStream.value);
      localStream.value = null;
    }
  }

  async function joinPendingInvite(video = false) {
    const client = xmppClient.value;
    const waddleId = activeWaddleId.value;
    const channelId = activeChannelId.value;
    const invite = pendingInvite.value?.invite;
    if (!client || !waddleId || !channelId || !invite) return;
    if (!(await ensureNotInCall("accept this invite"))) return;

    error.value = "";
    try {
      const stream = await acquireLocalMedia(video);
      const actualVideo = stream.getVideoTracks().length > 0;
      localStream.value = stream;

      const sessionInfo = await connectToMediaRoom(
        client,
        waddleId,
        channelId,
        stream,
        actualVideo,
      );

      sid.value = invite.inviteId;
      serviceJid.value = sessionInfo.roomName;
      trackRecoverableCall(invite.inviteId, actualVideo);
      pendingInvite.value = null;
      applyConnectedPhase("dialing");
    } catch (err) {
      await disconnectRoom();
      phase.value = "error";
      error.value = normalizeCallError(err, normalizeError);
      stopStream(localStream.value);
      localStream.value = null;
    }
  }

  async function switchToPendingInvite(video = false) {
    phase.value = "switching";
    await leaveCurrentCall({ clearPendingInvite: false });
    await joinPendingInvite(video);
  }

  async function declineInvite() {
    const invite = pendingInvite.value?.invite;
    const client = xmppClient.value;
    const waddleId = activeWaddleId.value;
    const channelId = activeChannelId.value;
    if (invite && client && waddleId && channelId) {
      try {
        await client.sendCallReject(waddleId, channelId, invite.inviteId);
      } catch {
        // best-effort
      }
    }
    dismissInvite();
  }

  function dismissInvite() {
    pendingInvite.value = null;
    if (phase.value === "ringing") {
      phase.value = recoverableCall.value ? "dialing" : "idle";
    }
  }

  async function toggleMicrophone() {
    const stream = localStream.value;
    if (!stream || !room) return;

    try {
      const nextEnabled = !micEnabled.value;
      await room.localParticipant.setMicrophoneEnabled(nextEnabled);
      for (const track of stream.getAudioTracks()) {
        track.enabled = nextEnabled;
      }
      micEnabled.value = nextEnabled;
    } catch (err) {
      error.value = normalizeCallError(err, normalizeError);
    }
  }

  async function toggleCamera() {
    const stream = localStream.value;
    if (!stream || !room) return;

    try {
      const publication = room.localParticipant.getTrackPublication(Track.Source.Camera);
      if (publication?.track) {
        await room.localParticipant.unpublishTrack(publication.track);
        for (const track of stream.getVideoTracks()) {
          stream.removeTrack(track);
          track.stop();
        }
        cameraEnabled.value = false;
        if (recoverableCall.value) {
          recoverableCall.value = { ...recoverableCall.value, video: false };
        }
        return;
      }

      const videoStream = await navigator.mediaDevices.getUserMedia({ video: true });
      const videoTrack = videoStream.getVideoTracks()[0];
      if (!videoTrack) {
        throw new Error("No camera track is available for this call.");
      }

      stream.addTrack(videoTrack);
      await room.localParticipant.publishTrack(videoTrack, {
        source: Track.Source.Camera,
      });
      cameraEnabled.value = true;
      if (recoverableCall.value) {
        recoverableCall.value = { ...recoverableCall.value, video: true };
      }
    } catch (err) {
      error.value = normalizeCallError(err, normalizeError);
    }
  }

  async function attemptRejoin() {
    if (rejoinPromise) return rejoinPromise;

    const call = recoverableCall.value;
    const client = xmppClient.value;
    if (!call || !client) return;
    if (activeWaddleId.value !== call.waddleId || activeChannelId.value !== call.channelId) return;
    if (xmppStatus.value.state !== "online") return;

    rejoinPromise = (async () => {
      error.value = "";
      phase.value = "reconnecting";

      try {
        let stream = localStream.value;
        const hasLiveTracks = !!stream?.getTracks().some((track) => track.readyState === "live");
        if (!hasLiveTracks) {
          stream = await acquireLocalMedia(call.video);
          localStream.value = stream;
        }

        await disconnectRoom();
        const actualVideo = stream!.getVideoTracks().length > 0;
        const sessionInfo = await connectToMediaRoom(
          client,
          call.waddleId,
          call.channelId,
          stream!,
          actualVideo,
        );

        sid.value = call.inviteId;
        serviceJid.value = sessionInfo.roomName;
        trackRecoverableCall(call.inviteId, actualVideo);
        applyConnectedPhase("dialing");
      } catch (err) {
        phase.value = "error";
        error.value = normalizeCallError(err, normalizeError);
      }
    })();

    try {
      await rejoinPromise;
    } finally {
      rejoinPromise = null;
    }
  }

  async function endCall() {
    await leaveCurrentCall();
  }

  watch(xmppClient, (client) => {
    if (!client) {
      void leaveCurrentCall();
    }
  });

  watch(latestCallInvite, (event) => {
    if (!event) return;
    if (sid.value && event.invite.inviteId === sid.value) return;

    pendingInvite.value = {
      roomJid: event.roomJid,
      fromNick: event.nick,
      invite: event.invite,
    };

    if (!recoverableCall.value) {
      phase.value = "ringing";
    }
  });

  watch([activeWaddleId, activeChannelId], ([nextWaddleId, nextChannelId], [prevWaddleId, prevChannelId]) => {
    if (nextWaddleId !== prevWaddleId || nextChannelId !== prevChannelId) {
      pendingInvite.value = null;
    }

    if (!inCall.value) return;

    const switchedWaddle = prevWaddleId !== null && nextWaddleId !== prevWaddleId;
    const roomDeselected = !nextWaddleId || !nextChannelId;
    if (switchedWaddle || roomDeselected) {
      void leaveCurrentCall();
    }
  });

  watch(xmppStatus, (status) => {
    if (status.state === "reconnecting" && inCall.value && phase.value !== "ending" && phase.value !== "switching") {
      phase.value = "reconnecting";
      return;
    }

    if (status.state === "online" && phase.value === "reconnecting" && !room) {
      void attemptRejoin();
    }
  });

  watch(session, (value) => {
    if (!value) {
      void leaveCurrentCall();
    }
  });

  return {
    phase,
    sid,
    serviceJid,
    error,
    inCall,
    isSwitching,
    localStream,
    remoteParticipants,
    pendingInvite,
    hasRemoteTracks,
    micEnabled,
    cameraEnabled,
    startAudioCall: () => startCall(false),
    startVideoCall: () => startCall(true),
    joinPendingInvite,
    switchToPendingInvite,
    declineInvite,
    dismissInvite,
    toggleMicrophone,
    toggleCamera,
    endCall,
  };
}
