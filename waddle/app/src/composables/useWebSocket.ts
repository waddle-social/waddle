import { ref, onUnmounted } from 'vue';
import { useMachine } from '@xstate/vue';
import { connectionManagerActor } from '../machines/actors/connectionManagerActor';

export function useWebSocket() {
  const { snapshot: connectionSnapshot, send: connectionSend } = useMachine(connectionManagerActor);
  
  const isConnected = ref(false);
  const isConnecting = ref(false);
  const connectionError = ref<string | null>(null);
  const messageQueue = ref<any[]>([]);

  // Connect to WebSocket
  const connect = (url: string) => {
    connectionSend({ type: 'CONNECT', url });
  };

  // Disconnect from WebSocket
  const disconnect = () => {
    connectionSend({ type: 'DISCONNECT' });
  };

  // Send message through WebSocket
  const sendMessage = (data: any) => {
    connectionSend({ type: 'SEND_MESSAGE', data });
  };

  // Force reconnection
  const reconnect = () => {
    connectionSend({ type: 'RECONNECT' });
  };

  // Send heartbeat
  const heartbeat = () => {
    connectionSend({ type: 'HEARTBEAT' });
  };

  // Set up heartbeat interval
  let heartbeatInterval: NodeJS.Timeout | null = null;
  
  const startHeartbeat = (intervalMs = 30000) => {
    if (heartbeatInterval) {
      clearInterval(heartbeatInterval);
    }
    
    heartbeatInterval = setInterval(() => {
      if (isConnected.value) {
        heartbeat();
        sendMessage({ type: 'ping', timestamp: Date.now() });
      }
    }, intervalMs);
  };

  const stopHeartbeat = () => {
    if (heartbeatInterval) {
      clearInterval(heartbeatInterval);
      heartbeatInterval = null;
    }
  };

  // Update reactive refs based on connection state
  const updateConnectionState = () => {
    const status = connectionSnapshot.value?.context?.connectionStatus;
    isConnected.value = status === 'connected';
    isConnecting.value = status === 'connecting' || status === 'reconnecting';
    connectionError.value = connectionSnapshot.value?.context?.error || null;
    messageQueue.value = connectionSnapshot.value?.context?.messageQueue || [];
  };

  // Watch for connection state changes
  const connectionWatcher = () => {
    updateConnectionState();
    
    if (isConnected.value) {
      startHeartbeat();
    } else {
      stopHeartbeat();
    }
  };

  // Set up connection state watching
  // Note: In a real implementation, you'd want to watch the snapshot properly
  setInterval(connectionWatcher, 100);

  // Cleanup on unmount
  onUnmounted(() => {
    stopHeartbeat();
    disconnect();
  });

  return {
    // State
    isConnected,
    isConnecting,
    connectionError,
    messageQueue,
    connectionSnapshot,
    
    // Actions
    connect,
    disconnect,
    sendMessage,
    reconnect,
    heartbeat,
    startHeartbeat,
    stopHeartbeat,
    
    // Raw connection send function for advanced usage
    connectionSend,
  };
}

// Specialized composable for chat WebSocket
export function useChatWebSocket() {
  const websocket = useWebSocket();
  
  const sendChatMessage = (message: {
    content: string;
    category: string;
    username: string;
  }) => {
    websocket.sendMessage({
      type: 'chat_message',
      ...message,
      timestamp: Date.now(),
      id: `msg_${Date.now()}_${Math.random().toString(36).substr(2, 9)}`,
    });
  };

  const joinRoom = (username: string, roomId = 'global') => {
    websocket.sendMessage({
      type: 'join_room',
      username,
      roomId,
      timestamp: Date.now(),
    });
  };

  const leaveRoom = (username: string, roomId = 'global') => {
    websocket.sendMessage({
      type: 'leave_room',
      username,
      roomId,
      timestamp: Date.now(),
    });
  };

  const sendTypingIndicator = (username: string, isTyping: boolean) => {
    websocket.sendMessage({
      type: 'typing_indicator',
      username,
      isTyping,
      timestamp: Date.now(),
    });
  };

  return {
    ...websocket,
    sendChatMessage,
    joinRoom,
    leaveRoom,
    sendTypingIndicator,
  };
}