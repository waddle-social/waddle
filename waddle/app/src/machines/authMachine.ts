import { setup, assign } from 'xstate';

export interface AuthContext {
  username: string;
  error: string | null;
}

export type AuthEvent = 
  | { type: 'LOGIN'; username: string }
  | { type: 'LOGOUT' }
  | { type: 'LOGIN_SUCCESS' }
  | { type: 'LOGIN_ERROR'; error: string };

export const authMachine = setup({
  types: {
    context: {} as AuthContext,
    events: {} as AuthEvent,
  },
  actions: {
    setUsername: assign({
      username: ({ event }) => {
        if (event.type === 'LOGIN') {
          return event.username;
        }
        return '';
      },
    }),
    setError: assign({
      error: ({ event }) => {
        if (event.type === 'LOGIN_ERROR') {
          return event.error;
        }
        return null;
      },
    }),
    clearError: assign({
      error: () => null,
    }),
    clearUsername: assign({
      username: () => '',
    }),
  },
}).createMachine({
  id: 'auth',
  initial: 'idle',
  context: {
    username: '',
    error: null,
  },
  states: {
    idle: {
      on: {
        LOGIN: {
          target: 'authenticating',
          actions: ['setUsername', 'clearError'],
        },
      },
    },
    authenticating: {
      on: {
        LOGIN_SUCCESS: {
          target: 'authenticated',
          actions: 'clearError',
        },
        LOGIN_ERROR: {
          target: 'idle',
          actions: ['setError', 'clearUsername'],
        },
      },
    },
    authenticated: {
      on: {
        LOGOUT: {
          target: 'idle',
          actions: ['clearUsername', 'clearError'],
        },
      },
    },
  },
});