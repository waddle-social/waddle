import { describe, expect, test } from 'bun:test';
import {
  buildConversationUpdateFromMessage,
  clearConversationUnreadCount,
  type ConversationSummaryLike,
} from './conversationState';

function message(
  from: string,
  to: string,
  body: string,
  messageType = 'chat',
) {
  return {
    from,
    to,
    body,
    timestamp: '2026-02-19T10:00:00.000Z',
    messageType,
  };
}

function summary(overrides: Partial<ConversationSummaryLike> = {}): ConversationSummaryLike {
  return {
    jid: 'alice@example.com',
    title: 'alice',
    preview: '',
    updatedAt: null,
    unreadCount: 0,
    ...overrides,
  };
}

describe('buildConversationUpdateFromMessage', () => {
  test('incoming DM from unknown sender creates conversation with unread count', () => {
    const update = buildConversationUpdateFromMessage(
      message('alice@example.com/device', 'me@example.com', 'Hello there'),
      {
        selfJid: 'me@example.com',
        activeConversationJid: null,
        existing: null,
        titleFromJid: (jid) => jid.split('@')[0] || jid,
      },
    );

    expect(update).not.toBeNull();
    expect(update?.summary.jid).toBe('alice@example.com');
    expect(update?.summary.unreadCount).toBe(1);
    expect(update?.isIncoming).toBe(true);
  });

  test('incoming DM on active conversation does not increment unread', () => {
    const update = buildConversationUpdateFromMessage(
      message('alice@example.com', 'me@example.com', 'Still here'),
      {
        selfJid: 'me@example.com',
        activeConversationJid: 'alice@example.com',
        existing: summary({ unreadCount: 3 }),
        titleFromJid: (jid) => jid.split('@')[0] || jid,
      },
    );

    expect(update).not.toBeNull();
    expect(update?.summary.unreadCount).toBe(3);
  });

  test('outgoing DM does not increment unread', () => {
    const update = buildConversationUpdateFromMessage(
      message('me@example.com', 'bob@example.com', 'Ping'),
      {
        selfJid: 'me@example.com',
        activeConversationJid: null,
        existing: summary({
          jid: 'bob@example.com',
          title: 'bob',
          unreadCount: 4,
        }),
        titleFromJid: (jid) => jid.split('@')[0] || jid,
      },
    );

    expect(update).not.toBeNull();
    expect(update?.summary.jid).toBe('bob@example.com');
    expect(update?.summary.unreadCount).toBe(4);
    expect(update?.isIncoming).toBe(false);
  });
});

describe('clearConversationUnreadCount', () => {
  test('clears unread for the selected conversation only', () => {
    const updated = clearConversationUnreadCount(
      [
        summary({ jid: 'alice@example.com', unreadCount: 2 }),
        summary({ jid: 'bob@example.com', unreadCount: 5 }),
      ],
      'alice@example.com/mobile',
    );

    expect(updated[0]?.unreadCount).toBe(0);
    expect(updated[1]?.unreadCount).toBe(5);
  });
});
