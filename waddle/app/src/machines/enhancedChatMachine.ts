import { setup, assign, spawnChild, stopChild } from 'xstate';
import { messageActor, type MessageData } from './actors/messageActor';

export interface EnhancedChatContext {
  messages: Map<string, MessageData>;
  messageActors: Map<string, any>; // ActorRef type would be better with proper typing
  pendingMessages: Set<string>;
  failedMessages: Set<string>;
  connectionStatus: 'disconnected' | 'connecting' | 'connected';
  error: string | null;
  username: string;
}

export type EnhancedChatEvent = 
  | { type: 'CONNECT'; username: string }
  | { type: 'DISCONNECT' }
  | { type: 'CONNECTION_SUCCESS' }
  | { type: 'CONNECTION_ERROR'; error: string }
  | { type: 'SEND_MESSAGE'; content: string; category: string }
  | { type: 'RECEIVE_MESSAGE'; message: MessageData }
  | { type: 'RETRY_MESSAGE'; messageId: string }
  | { type: 'DELETE_MESSAGE'; messageId: string }
  | { type: 'MESSAGE_SENT'; messageId: string }
  | { type: 'MESSAGE_FAILED'; messageId: string };

export const enhancedChatMachine = setup({
  types: {
    context: {} as EnhancedChatContext,
    events: {} as EnhancedChatEvent,
  },
  actions: {
    setUsername: assign({
      username: (_, params: { username: string }) => params.username,
    }),
    setConnecting: assign({
      connectionStatus: () => 'connecting',
    }),
    setConnected: assign({
      connectionStatus: () => 'connected',
    }),
    setDisconnected: assign({
      connectionStatus: () => 'disconnected',
    }),
    setError: assign({
      error: (_, params: { error: string }) => params.error,
    }),
    clearError: assign({
      error: () => null,
    }),
    addMessage: assign({
      messages: ({ context }, params: { message: MessageData }) => {
        const newMessages = new Map(context.messages);
        newMessages.set(params.message.id, params.message);
        return newMessages;
      },
    }),
    spawnMessageActor: assign({
      messageActors: ({ context, spawn }, params: { message: MessageData }) => {
        const newActors = new Map(context.messageActors);
        const actorRef = spawn(messageActor, {
          id: params.message.id,
          input: params.message,
        });
        newActors.set(params.message.id, actorRef);
        return newActors;
      },
      pendingMessages: ({ context }, params: { message: MessageData }) => {
        const newPending = new Set(context.pendingMessages);
        newPending.add(params.message.id);
        return newPending;
      },
    }),
    sendMessageToActor: ({ context }, params: { messageId: string }) => {
      const actor = context.messageActors.get(params.messageId);
      if (actor) {
        actor.send({ type: 'SEND' });
      }
    },
    markMessageSent: assign({
      pendingMessages: ({ context }, params: { messageId: string }) => {
        const newPending = new Set(context.pendingMessages);
        newPending.delete(params.messageId);
        return newPending;
      },
      failedMessages: ({ context }, params: { messageId: string }) => {
        const newFailed = new Set(context.failedMessages);
        newFailed.delete(params.messageId);
        return newFailed;
      },
    }),
    markMessageFailed: assign({
      pendingMessages: ({ context }, params: { messageId: string }) => {
        const newPending = new Set(context.pendingMessages);
        newPending.delete(params.messageId);
        return newPending;
      },
      failedMessages: ({ context }, params: { messageId: string }) => {
        const newFailed = new Set(context.failedMessages);
        newFailed.add(params.messageId);
        return newFailed;
      },
    }),
    removeMessage: assign({
      messages: ({ context }, params: { messageId: string }) => {
        const newMessages = new Map(context.messages);
        newMessages.delete(params.messageId);
        return newMessages;
      },
      messageActors: ({ context }, params: { messageId: string }) => {
        const newActors = new Map(context.messageActors);
        newActors.delete(params.messageId);
        return newActors;
      },
      pendingMessages: ({ context }, params: { messageId: string }) => {
        const newPending = new Set(context.pendingMessages);
        newPending.delete(params.messageId);
        return newPending;
      },
      failedMessages: ({ context }, params: { messageId: string }) => {
        const newFailed = new Set(context.failedMessages);
        newFailed.delete(params.messageId);
        return newFailed;
      },
    }),
    stopMessageActor: stopChild(({ context }, params: { messageId: string }) => 
      context.messageActors.get(params.messageId)
    ),
  },
}).createMachine({
  id: 'enhancedChat',
  initial: 'disconnected',
  context: {
    messages: new Map(),
    messageActors: new Map(),
    pendingMessages: new Set(),
    failedMessages: new Set(),
    connectionStatus: 'disconnected',
    error: null,
    username: '',
  },
  states: {
    disconnected: {
      on: {
        CONNECT: {
          target: 'connecting',
          actions: [
            { type: 'setUsername', params: ({ event }) => ({ username: event.username }) },
            'setConnecting',
            'clearError',
          ],
        },
      },
    },
    connecting: {
      on: {
        CONNECTION_SUCCESS: {
          target: 'connected',
          actions: ['setConnected'],
        },
        CONNECTION_ERROR: {
          target: 'disconnected',
          actions: [
            { type: 'setError', params: ({ event }) => ({ error: event.error }) },
            'setDisconnected',
          ],
        },
      },
    },
    connected: {
      on: {
        DISCONNECT: {
          target: 'disconnected',
          actions: ['setDisconnected'],
        },
        SEND_MESSAGE: {
          actions: [
            assign(({ context, event, spawn }) => {
              const messageData: MessageData = {
                id: `msg_${Date.now()}_${Math.random().toString(36).substr(2, 9)}`,
                content: event.content,
                category: event.category as any,
                username: context.username,
                timestamp: Date.now(),
              };
              
              // Update messages
              const newMessages = new Map(context.messages);
              newMessages.set(messageData.id, messageData);
              
              // Spawn and store actor
              const newActors = new Map(context.messageActors);
              const actorRef = spawn(messageActor, {
                id: messageData.id,
                input: messageData,
              });
              newActors.set(messageData.id, actorRef);
              
              // Add to pending
              const newPending = new Set(context.pendingMessages);
              newPending.add(messageData.id);
              
              // Send message to actor
              setTimeout(() => actorRef.send({ type: 'SEND' }), 0);
              
              return {
                messages: newMessages,
                messageActors: newActors,
                pendingMessages: newPending,
              };
            }),
          ],
        },
        RECEIVE_MESSAGE: {
          actions: {
            type: 'addMessage',
            params: ({ event }) => ({ message: event.message }),
          },
        },
        RETRY_MESSAGE: {
          actions: {
            type: 'sendMessageToActor',
            params: ({ event }) => ({ messageId: event.messageId }),
          },
        },
        DELETE_MESSAGE: {
          actions: [
            { type: 'stopMessageActor', params: ({ event }) => ({ messageId: event.messageId }) },
            { type: 'removeMessage', params: ({ event }) => ({ messageId: event.messageId }) },
          ],
        },
        MESSAGE_SENT: {
          actions: {
            type: 'markMessageSent',
            params: ({ event }) => ({ messageId: event.messageId }),
          },
        },
        MESSAGE_FAILED: {
          actions: {
            type: 'markMessageFailed',
            params: ({ event }) => ({ messageId: event.messageId }),
          },
        },
      },
    },
  },
});