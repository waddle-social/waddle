import { ref } from 'vue';
import { defineStore } from 'pinia';

import {
  useWaddle,
  type ChatMessage,
  type ConnectionSnapshot,
  type UnlistenFn,
} from '../composables/useWaddle';

export type ConnectionStatus = 'connecting' | 'connected' | 'reconnecting' | 'offline';
export type MessageDeliveryStatus = 'queued' | 'sent' | 'delivered';

interface EventPayloadEnvelope {
  type?: string;
  data?: Record<string, unknown>;
}

interface BackendEventEnvelope {
  channel?: string;
  payload?: EventPayloadEnvelope;
}

export const useRuntimeStore = defineStore('runtime', () => {
  const connectionStatus = ref<ConnectionStatus>('offline');
  const connectionMessage = ref<string | null>(null);
  const reconnectAttempt = ref<number | null>(null);

  const messageDeliveryById = ref<Record<string, MessageDeliveryStatus>>({});

  let bootstrapped = false;
  const unlistenFns: UnlistenFn[] = [];

  function setMessageDelivery(id: string, status: MessageDeliveryStatus): void {
    if (!id) return;
    messageDeliveryById.value = { ...messageDeliveryById.value, [id]: status };
  }

  function clearMessageDelivery(id: string): void {
    if (!id || !messageDeliveryById.value[id]) return;
    const next = { ...messageDeliveryById.value };
    delete next[id];
    messageDeliveryById.value = next;
  }

  function deliveryFor(messageId: string): MessageDeliveryStatus | null {
    return messageDeliveryById.value[messageId] ?? null;
  }

  function setConnected(jid: string | null): void {
    connectionStatus.value = 'connected';
    reconnectAttempt.value = null;
    connectionMessage.value = jid ? `Connected as ${jid}` : 'Connected';
  }

  function setReconnecting(attempt: number | null): void {
    connectionStatus.value = 'reconnecting';
    reconnectAttempt.value = attempt;
    connectionMessage.value = attempt ? `Reconnecting (attempt ${attempt})` : 'Reconnecting';
  }

  function setOffline(reason: string | null): void {
    connectionStatus.value = 'offline';
    reconnectAttempt.value = null;
    connectionMessage.value = reason || 'Offline';
  }

  function setConnecting(message: string | null = null): void {
    connectionStatus.value = 'connecting';
    reconnectAttempt.value = null;
    connectionMessage.value = message || 'Connecting…';
  }

  function applyConnectionSnapshot(snapshot: ConnectionSnapshot): void {
    switch (snapshot.status) {
      case 'connected':
        setConnected(snapshot.jid);
        return;
      case 'reconnecting':
        setReconnecting(snapshot.attempt);
        return;
      case 'offline':
        setOffline('Offline');
        return;
      case 'connecting':
      default:
        setConnecting();
    }
  }

  function handleSystemEvent(envelope: BackendEventEnvelope): void {
    const payload = envelope.payload;
    if (!payload?.type) return;

    switch (payload.type) {
      case 'connectionEstablished': {
        const jid = typeof payload.data?.jid === 'string' ? payload.data.jid : null;
        setConnected(jid);
        return;
      }
      case 'connectionReconnecting': {
        const attemptValue = payload.data?.attempt;
        const attempt = typeof attemptValue === 'number' ? attemptValue : null;
        setReconnecting(attempt);
        return;
      }
      case 'comingOnline': {
        if (connectionStatus.value !== 'connected') setConnecting();
        return;
      }
      case 'connectionLost': {
        const reason = typeof payload.data?.reason === 'string' ? payload.data.reason : null;
        setOffline(reason);
        return;
      }
      case 'goingOffline': {
        setOffline('Offline');
        return;
      }
      case 'errorOccurred': {
        const message = typeof payload.data?.message === 'string' ? payload.data.message : null;
        if (message) connectionMessage.value = message;
        return;
      }
      default:
        return;
    }
  }

  function handleMessageSent(envelope: BackendEventEnvelope): void {
    const payload = envelope.payload;
    if (payload?.type !== 'messageSent') return;
    const message = payload.data?.message as ChatMessage | undefined;
    if (message?.id) setMessageDelivery(message.id, 'sent');
  }

  function handleMessageDelivered(envelope: BackendEventEnvelope): void {
    const payload = envelope.payload;
    if (payload?.type !== 'messageDelivered') return;
    const id = typeof payload.data?.id === 'string' ? payload.data.id : null;
    if (id) setMessageDelivery(id, 'delivered');
  }

  async function bootstrap(): Promise<void> {
    if (bootstrapped) return;
    bootstrapped = true;

    const waddle = useWaddle();

    const subscriptions = [
      'system.connection.established',
      'system.connection.reconnecting',
      'system.connection.lost',
      'system.coming_online',
      'system.going_offline',
      'system.error.occurred',
      'xmpp.message.sent',
      'xmpp.message.delivered',
    ];

    for (const channel of subscriptions) {
      try {
        const unlisten = await waddle.listen<BackendEventEnvelope>(channel, ({ payload }) => {
          if (channel.startsWith('system.')) {
            handleSystemEvent(payload);
            return;
          }
          if (channel === 'xmpp.message.sent') {
            handleMessageSent(payload);
            return;
          }
          if (channel === 'xmpp.message.delivered') {
            handleMessageDelivered(payload);
          }
        });
        unlistenFns.push(unlisten);
      } catch {
        // Transport not ready yet — tolerate gracefully
      }
    }

    try {
      const snapshot = await waddle.getConnectionState();
      applyConnectionSnapshot(snapshot);
    } catch {
      // Not connected yet — stay in 'offline' state
      setOffline('Not signed in');
    }
  }

  function shutdown(): void {
    while (unlistenFns.length > 0) {
      const unlisten = unlistenFns.pop();
      unlisten?.();
    }
    bootstrapped = false;
    connectionStatus.value = 'offline';
    connectionMessage.value = null;
    reconnectAttempt.value = null;
    messageDeliveryById.value = {};
  }

  return {
    connectionStatus,
    connectionMessage,
    reconnectAttempt,
    messageDeliveryById,
    bootstrap,
    shutdown,
    setMessageDelivery,
    clearMessageDelivery,
    deliveryFor,
  };
});
