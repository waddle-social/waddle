import type { CallMedia } from "@/lib/calls/types";

export interface AudioAlertPlayer {
  startLoop(key: string): void | Promise<void>;
  stop(key: string): void | Promise<void>;
}

export interface MessageAudioAlertPlayer {
  play(key: string): void | Promise<void>;
}

interface IncomingCallNotificationHandle {
  close(): void;
}

export interface IncomingCallNotifier {
  showIncomingCall(options: {
    peerJid: string;
    sid: string;
    media: CallMedia;
    onClick: () => void;
  }): IncomingCallNotificationHandle | null;
}

export interface IncomingCallFocusTarget {
  focusConversation(peerJid: string, sid: string): void;
}

export interface IncomingCallAlertController {
  start(options: { peerJid: string; sid: string; media: CallMedia }): void;
  stop(sid: string): void;
  stopAll(): void;
}

export function createIncomingCallAlertController(options: {
  player: AudioAlertPlayer;
  notifier?: IncomingCallNotifier;
  focusTarget?: IncomingCallFocusTarget;
  isTabFocused?: () => boolean;
  // Presence Do Not Disturb (effective `<show>dnd</show>`): when true, the
  // whole incoming-call alert — looping ringtone + OS notification banner — is
  // silenced, mirroring the message-notification DND gate (ADR-010, #1075/#1081).
  // The in-app IncomingCallToast is `$callState`-driven and unaffected, so the
  // call stays visible/answerable; only the disturbance is suppressed.
  isDoNotDisturb?: () => boolean;
}): IncomingCallAlertController {
  const active = new Map<string, IncomingCallNotificationHandle | null>();

  return {
    start({ peerJid, sid, media }) {
      if (active.has(sid)) return;
      if (options.isDoNotDisturb?.() === true) return;
      active.set(sid, null);
      void options.player.startLoop(sid);
      if (options.isTabFocused?.() !== false) return;
      const handle = options.notifier?.showIncomingCall({
        peerJid,
        sid,
        media,
        onClick: () => options.focusTarget?.focusConversation(peerJid, sid),
      }) ?? null;
      active.set(sid, handle);
    },
    stop(sid) {
      if (!active.has(sid)) return;
      active.get(sid)?.close();
      active.delete(sid);
      void options.player.stop(sid);
    },
    stopAll() {
      for (const sid of [...active.keys()]) {
        this.stop(sid);
      }
    },
  };
}

export function createBrowserIncomingCallNotifier(): IncomingCallNotifier {
  return {
    showIncomingCall({ peerJid, media, onClick }) {
      if (typeof window === "undefined" || !("Notification" in window)) return null;
      if (Notification.permission !== "granted") return null;
      const label = media.video ? "video" : "audio";
      const notification = new Notification(`Incoming ${label} call`, {
        body: "Open Waddle to answer",
        tag: `call:${peerJid}`,
        icon: "/android-chrome-192x192.png",
      });
      notification.onclick = () => {
        window.focus();
        onClick();
        notification.close();
      };
      return notification;
    },
  };
}

export function createBrowserLoopingTonePlayer(): AudioAlertPlayer {
  const active = new Map<string, {
    context: AudioContext;
    oscillator: OscillatorNode;
    gain: GainNode;
    interval: ReturnType<typeof setInterval>;
  }>();

  return {
    async startLoop(key) {
      if (active.has(key)) return;
      if (typeof window === "undefined" || typeof AudioContext === "undefined") return;
      const context = new AudioContext();
      const oscillator = context.createOscillator();
      const gain = context.createGain();
      oscillator.type = "sine";
      oscillator.frequency.value = 880;
      gain.gain.value = 0;
      oscillator.connect(gain);
      gain.connect(context.destination);
      oscillator.start();
      const pulse = () => {
        const now = context.currentTime;
        gain.gain.cancelScheduledValues(now);
        gain.gain.setValueAtTime(0, now);
        gain.gain.linearRampToValueAtTime(0.08, now + 0.03);
        gain.gain.linearRampToValueAtTime(0, now + 0.35);
      };
      pulse();
      active.set(key, {
        context,
        oscillator,
        gain,
        interval: setInterval(pulse, 1400),
      });
      await context.resume().catch(() => undefined);
    },
    stop(key) {
      const current = active.get(key);
      if (!current) return;
      active.delete(key);
      clearInterval(current.interval);
      current.oscillator.stop();
      void current.context.close().catch(() => undefined);
    },
  };
}

export function createBrowserMessageTonePlayer(): MessageAudioAlertPlayer {
  const active = new Set<string>();

  return {
    async play(key) {
      if (active.has(key)) return;
      if (typeof window === "undefined" || typeof AudioContext === "undefined") return;

      active.add(key);
      let context: AudioContext | undefined;
      let cleanedUp = false;
      const cleanup = () => {
        if (cleanedUp) return;
        cleanedUp = true;
        active.delete(key);
        void context?.close().catch(() => undefined);
      };

      try {
        context = new AudioContext();
        const oscillator = context.createOscillator();
        const gain = context.createGain();
        oscillator.type = "sine";
        oscillator.frequency.value = 1046.5;
        gain.gain.value = 0;
        oscillator.connect(gain);
        gain.connect(context.destination);
        oscillator.onended = cleanup;
        await context.resume();
        const now = context.currentTime;
        gain.gain.setValueAtTime(0, now);
        gain.gain.linearRampToValueAtTime(0.06, now + 0.02);
        gain.gain.linearRampToValueAtTime(0, now + 0.18);
        oscillator.start(now);
        oscillator.stop(now + 0.22);
      } catch {
        cleanup();
      }
    },
  };
}
