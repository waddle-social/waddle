import { setup, assign } from 'xstate';
import type { Category } from './filterMachine';

export interface MessageContext {
  content: string;
  category: Category;
  error: string | null;
}

export type MessageEvent = 
  | { type: 'TYPE'; content: string }
  | { type: 'SET_CATEGORY'; category: Category }
  | { type: 'SEND' }
  | { type: 'SEND_SUCCESS' }
  | { type: 'SEND_ERROR'; error: string }
  | { type: 'CLEAR' };

export const messageMachine = setup({
  types: {
    context: {} as MessageContext,
    events: {} as MessageEvent,
  },
  actions: {
    setContent: assign({
      content: ({ event }) => {
        if (event.type === 'TYPE') {
          return event.content;
        }
        return '';
      },
    }),
    setCategory: assign({
      category: ({ event }) => {
        if (event.type === 'SET_CATEGORY') {
          return event.category;
        }
        return 'General';
      },
    }),
    setError: assign({
      error: ({ event }) => {
        if (event.type === 'SEND_ERROR') {
          return event.error;
        }
        return null;
      },
    }),
    clearMessage: assign({
      content: () => '',
      error: () => null,
    }),
    clearError: assign({
      error: () => null,
    }),
  },
}).createMachine({
  id: 'message',
  initial: 'idle',
  context: {
    content: '',
    category: 'General',
    error: null,
  },
  states: {
    idle: {
      on: {
        TYPE: {
          actions: 'setContent',
        },
        SET_CATEGORY: {
          actions: 'setCategory',
        },
        SEND: [
          {
            guard: ({ context }) => context.content.trim().length > 0,
            target: 'sending',
            actions: 'clearError',
          },
          {
            actions: assign({
              error: () => 'Message cannot be empty',
            }),
          },
        ],
      },
    },
    sending: {
      on: {
        SEND_SUCCESS: {
          target: 'idle',
          actions: 'clearMessage',
        },
        SEND_ERROR: {
          target: 'idle',
          actions: 'setError',
        },
      },
    },
  },
});