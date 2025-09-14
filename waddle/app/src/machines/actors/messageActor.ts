import { setup, assign, fromPromise } from 'xstate';
import type { Category } from '../filterMachine';

export interface MessageData {
  id: string;
  content: string;
  category: Category;
  username: string;
  timestamp: number;
}

export interface MessageActorContext {
  message: MessageData;
  retryCount: number;
  error: string | null;
}

export type MessageActorEvent = 
  | { type: 'SEND' }
  | { type: 'RETRY' }
  | { type: 'CANCEL' }
  | { type: 'MARK_READ' }
  | { type: 'DELETE' };

export const messageActor = setup({
  types: {
    context: {} as MessageActorContext,
    events: {} as MessageActorEvent,
    input: {} as MessageData,
  },
  actors: {
    sendMessage: fromPromise<void, { message: MessageData }>(async ({ input }) => {
      // Simulate network delay
      await new Promise(resolve => setTimeout(resolve, 500 + Math.random() * 1000));
      
      // Simulate occasional failures for demo
      if (Math.random() < 0.1) {
        throw new Error('Network error: Failed to send message');
      }
      
      // Here you would actually send to WebSocket/API
      console.log('Message sent:', input.message);
    }),
  },
  actions: {
    incrementRetryCount: assign({
      retryCount: ({ context }) => context.retryCount + 1,
    }),
    setError: assign({
      error: (_, params: { error: string }) => params.error,
    }),
    clearError: assign({
      error: () => null,
    }),
    notifyParent: ({ context }, params: { type: string }) => {
      // Send event to parent machine
      console.log(`Message ${context.message.id}: ${params.type}`);
    },
  },
  guards: {
    canRetry: ({ context }) => context.retryCount < 3,
    shouldAutoRetry: ({ context }) => context.retryCount < 2,
  },
}).createMachine({
  id: 'messageActor',
  initial: 'idle',
  context: ({ input }) => ({
    message: input,
    retryCount: 0,
    error: null,
  }),
  states: {
    idle: {
      on: {
        SEND: 'sending',
      },
    },
    sending: {
      invoke: {
        src: 'sendMessage',
        input: ({ context }) => ({ message: context.message }),
        onDone: {
          target: 'sent',
          actions: [
            'clearError',
            { type: 'notifyParent', params: { type: 'MESSAGE_SENT' } },
          ],
        },
        onError: [
          {
            guard: 'shouldAutoRetry',
            target: 'retrying',
            actions: [
              'incrementRetryCount',
              { type: 'setError', params: { error: 'Send failed, retrying...' } },
            ],
          },
          {
            target: 'failed',
            actions: [
              'incrementRetryCount',
              { type: 'setError', params: { error: 'Failed to send message' } },
              { type: 'notifyParent', params: { type: 'MESSAGE_FAILED' } },
            ],
          },
        ],
      },
    },
    retrying: {
      after: {
        2000: 'sending', // Retry after 2 seconds
      },
      on: {
        CANCEL: 'failed',
      },
    },
    sent: {
      on: {
        MARK_READ: 'read',
        DELETE: 'deleted',
      },
    },
    failed: {
      on: {
        RETRY: {
          guard: 'canRetry',
          target: 'sending',
          actions: 'clearError',
        },
        DELETE: 'deleted',
      },
    },
    read: {
      on: {
        DELETE: 'deleted',
      },
    },
    deleted: {
      type: 'final',
    },
  },
});