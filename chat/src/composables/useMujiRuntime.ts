import { computed, ref, watch, type Ref } from "vue";
import type { WaddleSession } from "@/lib/server-auth";
import type { ActiveCallSummary, WaddleApi } from "@/lib/waddle-api";
import type {
  BrowserXmppClient,
  CallInviteInfo,
  IncomingCallInviteEvent,
  MujiCallEvent,
  MujiCallPhase,
  XmppStatusSnapshot,
} from "@/lib/xmpp-client";

interface PendingInvite {
  roomJid?: string;
  fromNick: string;
  invite: CallInviteInfo;
}

interface RecoverableCall {
  sid: string;
  serviceJid: string;
  waddleId: string;
  channelId: string;
  video: boolean;
}

function stopStream(stream: MediaStream | null) {
  if (!stream) return;
  for (const track of stream.getTracks()) track.stop();
}

export function useMujiRuntime(
  session: Ref<WaddleSession | null>,
  api: Ref<WaddleApi | null>,
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
  const remoteStream = ref<MediaStream | null>(null);
  const pendingInvite = ref<PendingInvite | null>(null);
  const micEnabled = ref(true);
  const cameraEnabled = ref(true);
  const hasRemoteTracks = ref(false);
  const activeCalls = ref<ActiveCallSummary[]>([]);
  const isLoadingActiveCalls = ref(false);
  const joiningListedCallId = ref<string | null>(null);
  const recoverableCall = ref<RecoverableCall | null>(null);
  let leavePromise: Promise<void> | null = null;
  let rejoinPromise: Promise<void> | null = null;

  const inCall = computed(() => phase.value !== "idle" && phase.value !== "error");
  const isSwitching = computed(() => phase.value === "switching");

  function clearRemoteMediaState() {
    sid.value = null;
    serviceJid.value = null;
    hasRemoteTracks.value = false;
    stopStream(remoteStream.value);
    remoteStream.value = null;
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

  function trackRecoverableCall(nextSid: string, nextServiceJid: string, video: boolean) {
    const waddleId = activeWaddleId.value;
    const channelId = activeChannelId.value;
    if (!waddleId || !channelId) return;
    recoverableCall.value = {
      sid: nextSid,
      serviceJid: nextServiceJid,
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
    return navigator.mediaDevices.getUserMedia({ audio: true, video });
  }

  function applyMujiEvent(event: MujiCallEvent) {
    if (event.type === "incoming") {
      if (!pendingInvite.value) {
        pendingInvite.value = {
          fromNick: event.peerJid.split("@")[0] ?? "unknown",
          invite: { inviteId: event.sid, muji: true, jingleSid: event.sid, jingleJid: event.peerJid },
        };
      }
      if (!inCall.value) {
        phase.value = "ringing";
      }
      sid.value = event.sid;
      serviceJid.value = event.peerJid;
      return;
    }
    if (event.type === "outgoing") {
      phase.value = "dialing";
      sid.value = event.sid;
      serviceJid.value = event.peerJid;
      trackRecoverableCall(event.sid, event.peerJid, cameraEnabled.value);
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
        void refreshActiveCalls();
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
    if (sid.value && event.invite.jingleSid && event.invite.jingleSid === sid.value) return;
    pendingInvite.value = {
      roomJid: event.roomJid,
      fromNick: event.nick,
      invite: event.invite,
    };
  });

  async function leaveCurrentCall(opts: { clearPendingInvite?: boolean } = {}) {
    if (leavePromise) return leavePromise;
    const activeSid = sid.value;
    const context = recoverableCall.value;
    if (!activeSid && !context) {
      resetCallState();
      return;
    }

    const { clearPendingInvite = true } = opts;
    leavePromise = (async () => {
      phase.value = "ending";
      try {
        const client = xmppClient.value;
        const activeCallId = activeSid ?? context?.sid;
        if (api.value && activeCallId) {
          try {
            await api.value.leaveCall(activeCallId);
          } catch {
            // best-effort leave registration cleanup
          }
        }
        if (client && context) {
          try {
            await client.sendCallLeft(context.waddleId, context.channelId, activeCallId);
          } catch {
            // best-effort leave signal
          }
        }
        client?.endMujiCall(activeCallId);
      } finally {
        resetCallState();
        if (clearPendingInvite) pendingInvite.value = null;
        error.value = "";
        await refreshActiveCalls();
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
    const apiClient = api.value;
    const waddleId = activeWaddleId.value;
    const channelId = activeChannelId.value;
    if (!client || !waddleId || !channelId) return;
    if (!(await ensureNotInCall("start another one"))) return;
    error.value = "";
    try {
      const stream = await acquireLocalMedia(video);
      localStream.value = stream;
      let preferredSid: string | undefined;
      if (apiClient) {
        try {
          const createdSession = await apiClient.createMediaSession(channelId, "publisher");
          preferredSid = createdSession.call_id;
        } catch (err) {
          console.warn("Failed to register call session; falling back to invite-only flow", err);
        }
      }
      const result = await client.startMujiCall(
        waddleId,
        channelId,
        stream,
        preferredSid ? { video, sid: preferredSid } : { video },
      );
      if (!result) throw new Error("Failed to start Muji session.");
      sid.value = result.sid;
      serviceJid.value = result.serviceJid;
      trackRecoverableCall(result.sid, result.serviceJid, video);
      pendingInvite.value = null;
      phase.value = "dialing";
      void refreshActiveCalls();
    } catch (err) {
      phase.value = "error";
      error.value = normalizeError(err);
      stopStream(localStream.value);
      localStream.value = null;
    }
  }

  async function refreshActiveCalls() {
    const apiClient = api.value;
    const channelId = activeChannelId.value;
    if (!apiClient || !channelId) {
      activeCalls.value = [];
      return;
    }

    isLoadingActiveCalls.value = true;
    try {
      const response = await apiClient.listActiveCalls(channelId);
      activeCalls.value = response.calls;
    } catch {
      activeCalls.value = [];
    } finally {
      isLoadingActiveCalls.value = false;
    }
  }

  async function joinListedCall(callId: string, video = true) {
    const client = xmppClient.value;
    const apiClient = api.value;
    const waddleId = activeWaddleId.value;
    const channelId = activeChannelId.value;
    if (!client || !apiClient || !waddleId || !channelId) return;
    if (!(await ensureNotInCall("join a different call"))) return;

    joiningListedCallId.value = callId;
    error.value = "";
    try {
      const bootstrap = await apiClient.bootstrapCallJoin(callId, "subscriber");
      if (bootstrap.call.state !== "active") {
        throw new Error("This call is no longer active.");
      }
      if (!bootstrap.call.call_id) {
        throw new Error("Call bootstrap response is missing call_id.");
      }

      const stream = await acquireLocalMedia(video);
      localStream.value = stream;

      const result = await client.joinMujiCall(waddleId, channelId, stream, {
        sid: bootstrap.call.call_id,
        video,
      });
      if (!result) throw new Error("Failed to join listed call.");
      sid.value = result.sid;
      serviceJid.value = result.serviceJid;
      trackRecoverableCall(result.sid, result.serviceJid, video);
      pendingInvite.value = null;
      phase.value = "dialing";
      await refreshActiveCalls();
    } catch (err) {
      phase.value = "error";
      error.value = normalizeError(err);
      stopStream(localStream.value);
      localStream.value = null;
    } finally {
      joiningListedCallId.value = null;
    }
  }

  async function joinPendingInvite(video = true) {
    const client = xmppClient.value;
    const waddleId = activeWaddleId.value;
    const channelId = activeChannelId.value;
    const invite = pendingInvite.value?.invite;
    if (!client || !waddleId || !channelId || !invite) return;
    if (!(await ensureNotInCall("accept this invite"))) return;
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
      trackRecoverableCall(result.sid, result.serviceJid, video);
      pendingInvite.value = null;
      phase.value = "dialing";
    } catch (err) {
      phase.value = "error";
      error.value = normalizeError(err);
      stopStream(localStream.value);
      localStream.value = null;
    }
  }

  async function switchToListedCall(callId: string, video = true) {
    phase.value = "switching";
    await leaveCurrentCall({ clearPendingInvite: false });
    await joinListedCall(callId, video);
  }

  async function switchToPendingInvite(video = true) {
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
    if (recoverableCall.value) {
      recoverableCall.value = {
        ...recoverableCall.value,
        video: cameraEnabled.value,
      };
    }
  }

  async function attemptRejoin() {
    if (rejoinPromise) return rejoinPromise;
    const call = recoverableCall.value;
    const client = xmppClient.value;
    if (!call || !client) return;
    if (activeWaddleId.value !== call.waddleId || activeChannelId.value !== call.channelId) return;

    rejoinPromise = (async () => {
      if (phase.value !== "reconnecting") return;
      error.value = "";
      try {
        let stream = localStream.value;
        const hasLiveTrack = !!stream?.getTracks().some((track) => track.readyState === "live");
        if (!hasLiveTrack) {
          stream = await acquireLocalMedia(call.video);
          localStream.value = stream;
        }
        const result = await client.joinMujiCall(call.waddleId, call.channelId, stream!, {
          sid: call.sid,
          jingleJid: call.serviceJid,
          video: call.video,
        });
        if (!result) throw new Error("Failed to rejoin call after reconnect.");
        sid.value = result.sid;
        serviceJid.value = result.serviceJid;
        trackRecoverableCall(result.sid, result.serviceJid, call.video);
        phase.value = "dialing";
      } catch (err) {
        phase.value = "error";
        error.value = normalizeError(err);
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

  watch([activeWaddleId, activeChannelId], () => {
    pendingInvite.value = null;
    if (recoverableCall.value) {
      const sameRoom = recoverableCall.value.waddleId === activeWaddleId.value
        && recoverableCall.value.channelId === activeChannelId.value;
      if (!sameRoom) recoverableCall.value = null;
    }
    if (!inCall.value) return;
    void leaveCurrentCall();
  });

  watch(xmppStatus, (status) => {
    if (status.state === "offline" && (phase.value === "active" || phase.value === "dialing" || phase.value === "ringing")) {
      phase.value = "reconnecting";
      void xmppClient.value?.connect().catch(() => undefined);
      return;
    }
    if (status.state === "online" && phase.value === "reconnecting") {
      void attemptRejoin();
    }
  });

  watch([api, session, activeChannelId], ([apiClient, activeSession, channelId], _, onCleanup) => {
    activeCalls.value = [];
    if (!apiClient || !activeSession || !channelId) return;

    let cancelled = false;
    const runRefresh = async () => {
      if (cancelled) return;
      await refreshActiveCalls();
    };

    void runRefresh();
    if (typeof window === "undefined") return;

    const intervalId = window.setInterval(() => {
      void runRefresh();
    }, 15000);

    onCleanup(() => {
      cancelled = true;
      window.clearInterval(intervalId);
    });
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
    remoteStream,
    pendingInvite,
    activeCalls,
    isLoadingActiveCalls,
    joiningListedCallId,
    hasRemoteTracks,
    micEnabled,
    cameraEnabled,
    refreshActiveCalls,
    joinListedCall,
    switchToListedCall,
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
