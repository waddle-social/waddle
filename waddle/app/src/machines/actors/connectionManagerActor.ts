import { setup, assign, fromCallback } from 'xstate';

export interface ConnectionManagerContext {
  url: string;
  socket: WebSocket | null;
  reconnectAttempts: number;
  maxReconnectAttempts: number;
  reconnectDelay: number;
  maxReconnectDelay: number;
  messageQueue: any[];
  lastHeartbeat: number;
  error: string | null;
}

export type ConnectionManagerEvent = 
  | { type: 'CONNECT'; url: string }
  | { type: 'DISCONNECT' }
  | { type: 'SEND_MESSAGE'; data: any }
  | { type: 'SOCKET_OPEN' }
  | { type: 'SOCKET_CLOSE'; code: number; reason: string }
  | { type: 'SOCKET_ERROR'; error: any }
  | { type: 'SOCKET_MESSAGE'; data: any }
  | { type: 'HEARTBEAT' }
  | { type: 'RECONNECT' }
  | { type: 'FORCE_DISCONNECT' };

export const connectionManagerActor = setup({
  types: {
    context: {} as ConnectionManagerContext,
    events: {} as ConnectionManagerEvent,
  },
  actors: {
    websocketConnection: fromCallback<ConnectionManagerEvent, { url: string }>(
      ({ input, sendBack, receive }) => {
        let socket: WebSocket;
        
        try {
          socket = new WebSocket(input.url);
          
          socket.onopen = () => {
            sendBack({ type: 'SOCKET_OPEN' });
          };
          
          socket.onclose = (event) => {
            sendBack({ 
              type: 'SOCKET_CLOSE', 
              code: event.code, 
              reason: event.reason 
            });
          };
          
          socket.onerror = (error) => {
            sendBack({ type: 'SOCKET_ERROR', error });
          };
          
          socket.onmessage = (event) => {
            try {
              const data = JSON.parse(event.data);
              sendBack({ type: 'SOCKET_MESSAGE', data });
            } catch (error) {
              console.error('Failed to parse WebSocket message:', error);
            }
          };
          
          // Handle outgoing messages
          receive((event) => {
            if (event.type === 'SEND_MESSAGE' && socket.readyState === WebSocket.OPEN) {
              socket.send(JSON.stringify(event.data));
            }
          });
          
        } catch (error) {
          sendBack({ type: 'SOCKET_ERROR', error });
        }
        
        return () => {
          if (socket && socket.readyState === WebSocket.OPEN) {
            socket.close();
          }
        };
      }
    ),
  },
  actions: {
    setUrl: assign({
      url: (_, params: { url: string }) => params.url,
    }),
    setSocket: assign({
      socket: (_, params: { socket: WebSocket }) => params.socket,
    }),
    clearSocket: assign({
      socket: () => null,
    }),
    incrementReconnectAttempts: assign({
      reconnectAttempts: ({ context }) => context.reconnectAttempts + 1,
    }),
    resetReconnectAttempts: assign({
      reconnectAttempts: () => 0,
    }),
    increaseReconnectDelay: assign({
      reconnectDelay: ({ context }) => 
        Math.min(context.reconnectDelay * 1.5, context.maxReconnectDelay),
    }),
    resetReconnectDelay: assign({
      reconnectDelay: () => 1000,
    }),
    queueMessage: assign({
      messageQueue: ({ context }, params: { data: any }) => [
        ...context.messageQueue,
        params.data,
      ],
    }),
    clearMessageQueue: assign({
      messageQueue: () => [],
    }),
    updateHeartbeat: assign({
      lastHeartbeat: () => Date.now(),
    }),
    setError: assign({
      error: (_, params: { error: string }) => params.error,
    }),
    clearError: assign({
      error: () => null,
    }),
    notifyParent: (_, params: { type: string; data?: any }) => {
      // This would send events to parent machine
      console.log(`Connection Manager: ${params.type}`, params.data);
    },
    flushMessageQueue: ({ context }) => {
      // Send all queued messages when connection is restored
      if (context.socket && context.socket.readyState === WebSocket.OPEN) {
        context.messageQueue.forEach(message => {
          context.socket!.send(JSON.stringify(message));
        });
      }
    },
  },
  guards: {
    canReconnect: ({ context }) => 
      context.reconnectAttempts < context.maxReconnectAttempts,
    shouldReconnect: ({ event }) => {
      // Don't reconnect for certain close codes (1000 = normal closure, 1001 = going away)
      if (event.type === 'SOCKET_CLOSE') {
        return event.code !== 1000 && event.code !== 1001;
      }
      return true;
    },
  },
}).createMachine({
  id: 'connectionManager',
  initial: 'disconnected',
  context: {
    url: '',
    socket: null,
    reconnectAttempts: 0,
    maxReconnectAttempts: 5,
    reconnectDelay: 1000,
    maxReconnectDelay: 30000,
    messageQueue: [],
    lastHeartbeat: 0,
    error: null,
  },
  states: {
    disconnected: {
      entry: [
        'clearSocket',
        { type: 'notifyParent', params: { type: 'CONNECTION_STATUS_CHANGED', data: 'disconnected' } },
      ],
      on: {
        CONNECT: {
          target: 'connecting',
          actions: [
            { type: 'setUrl', params: ({ event }) => ({ url: event.url }) },
            'clearError',
          ],
        },
      },
    },
    connecting: {
      entry: [
        { type: 'notifyParent', params: { type: 'CONNECTION_STATUS_CHANGED', data: 'connecting' } },
      ],
      invoke: {
        src: 'websocketConnection',
        input: ({ context }) => ({ url: context.url }),
        onDone: 'disconnected',
      },
      on: {
        SOCKET_OPEN: {
          target: 'connected',
          actions: [
            'resetReconnectAttempts',
            'resetReconnectDelay',
            'updateHeartbeat',
            'flushMessageQueue',
            'clearMessageQueue',
            { type: 'notifyParent', params: { type: 'CONNECTION_SUCCESS' } },
          ],
        },
        SOCKET_ERROR: {
          target: 'reconnecting',
          actions: [
            { type: 'setError', params: ({ event }) => ({ error: event.error.toString() }) },
            'incrementReconnectAttempts',
          ],
        },
        SOCKET_CLOSE: [
          {
            guard: 'shouldReconnect',
            target: 'reconnecting',
            actions: [
              'incrementReconnectAttempts',
              { type: 'setError', params: ({ event }) => ({ 
                error: `Connection closed: ${event.code} - ${event.reason}` 
              }) },
            ],
          },
          {
            target: 'disconnected',
            actions: [
              { type: 'setError', params: ({ event }) => ({ 
                error: `Connection closed: ${event.code} - ${event.reason}` 
              }) },
            ],
          },
        ],
        DISCONNECT: 'disconnected',
        SEND_MESSAGE: {
          actions: {
            type: 'queueMessage',
            params: ({ event }) => ({ data: event.data }),
          },
        },
      },
    },
    connected: {
      entry: [
        { type: 'notifyParent', params: { type: 'CONNECTION_STATUS_CHANGED', data: 'connected' } },
      ],
      on: {
        SOCKET_MESSAGE: {
          actions: {
            type: 'notifyParent',
            params: ({ event }) => ({ type: 'MESSAGE_RECEIVED', data: event.data }),
          },
        },
        SOCKET_CLOSE: [
          {
            guard: 'shouldReconnect',
            target: 'reconnecting',
            actions: [
              'incrementReconnectAttempts',
              { type: 'setError', params: ({ event }) => ({ 
                error: `Connection lost: ${event.code} - ${event.reason}` 
              }) },
            ],
          },
          {
            target: 'disconnected',
          },
        ],
        SOCKET_ERROR: {
          target: 'reconnecting',
          actions: [
            'incrementReconnectAttempts',
            { type: 'setError', params: ({ event }) => ({ error: event.error.toString() }) },
          ],
        },
        SEND_MESSAGE: {
          // Message will be handled by the websocket callback
        },
        HEARTBEAT: {
          actions: 'updateHeartbeat',
        },
        DISCONNECT: 'disconnecting',
        FORCE_DISCONNECT: 'disconnected',
      },
    },
    reconnecting: {
      entry: [
        'increaseReconnectDelay',
        { type: 'notifyParent', params: { type: 'CONNECTION_STATUS_CHANGED', data: 'reconnecting' } },
      ],
      after: {
        // Use dynamic delay based on context
        1000: [
          {
            guard: 'canReconnect',
            target: 'connecting',
          },
          {
            target: 'disconnected',
            actions: [
              { type: 'setError', params: { error: 'Max reconnection attempts reached' } },
              { type: 'notifyParent', params: { type: 'CONNECTION_FAILED' } },
            ],
          },
        ],
      },
      on: {
        RECONNECT: {
          guard: 'canReconnect',
          target: 'connecting',
          actions: 'resetReconnectAttempts',
        },
        DISCONNECT: 'disconnected',
        SEND_MESSAGE: {
          actions: {
            type: 'queueMessage',
            params: ({ event }) => ({ data: event.data }),
          },
        },
      },
    },
    disconnecting: {
      entry: [
        { type: 'notifyParent', params: { type: 'CONNECTION_STATUS_CHANGED', data: 'disconnecting' } },
      ],
      after: {
        1000: 'disconnected',
      },
    },
  },
});