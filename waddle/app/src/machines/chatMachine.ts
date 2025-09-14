import { setup, assign } from 'xstate';

export interface Message {
  id: string;
  username: string;
  content: string;
  timestamp: number;
  category: string;
}

export interface ChatContext {
  messages: Message[];
  connectionStatus: 'disconnected' | 'connecting' | 'connected';
  error: string | null;
  socket: WebSocket | null;
}

export type ChatEvent = 
  | { type: 'CONNECT' }
  | { type: 'DISCONNECT' }
  | { type: 'CONNECTION_SUCCESS'; socket: WebSocket }
  | { type: 'CONNECTION_ERROR'; error: string }
  | { type: 'MESSAGE_RECEIVED'; message: Message }
  | { type: 'SEND_MESSAGE'; content: string; category: string }
  | { type: 'MESSAGE_SENT' }
  | { type: 'MESSAGE_ERROR'; error: string };

export const chatMachine = setup({
  types: {
    context: {} as ChatContext,
    events: {} as ChatEvent,
  },
  actions: {
    setSocket: assign({
      socket: ({ event }) => {
        if (event.type === 'CONNECTION_SUCCESS') {
          return event.socket;
        }
        return null;
      },
    }),
    addMessage: assign({
      messages: ({ context, event }) => {
        if (event.type === 'MESSAGE_RECEIVED') {
          return [...context.messages, event.message];
        }
        return context.messages;
      },
    }),
    setError: assign({
      error: ({ event }) => {
        if (event.type === 'CONNECTION_ERROR' || event.type === 'MESSAGE_ERROR') {
          return event.error;
        }
        return null;
      },
    }),
    clearError: assign({
      error: () => null,
    }),
    setConnecting: assign({
      connectionStatus: () => 'connecting',
    }),
    setConnected: assign({
      connectionStatus: () => 'connected',
    }),
    setDisconnected: assign({
      connectionStatus: () => 'disconnected',
      socket: () => null,
    }),
  },
}).createMachine({
  id: 'chat',
  initial: 'disconnected',
  context: {
    messages: [],
    connectionStatus: 'disconnected',
    error: null,
    socket: null,
  },
  states: {
    disconnected: {
      on: {
        CONNECT: {
          target: 'connecting',
          actions: 'setConnecting',
        },
      },
    },
    connecting: {
      on: {
        CONNECTION_SUCCESS: {
          target: 'connected',
          actions: ['setSocket', 'setConnected', 'clearError'],
        },
        CONNECTION_ERROR: {
          target: 'disconnected',
          actions: ['setError', 'setDisconnected'],
        },
      },
    },
    connected: {
      on: {
        DISCONNECT: {
          target: 'disconnected',
          actions: 'setDisconnected',
        },
        MESSAGE_RECEIVED: {
          actions: 'addMessage',
        },
        SEND_MESSAGE: {
          target: 'sending',
        },
      },
    },
    sending: {
      on: {
        MESSAGE_SENT: {
          target: 'connected',
        },
        MESSAGE_ERROR: {
          target: 'connected',
          actions: 'setError',
        },
      },
    },
  },
});