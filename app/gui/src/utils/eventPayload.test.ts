import { describe, expect, test } from 'bun:test';
import { extractMessageFromEventPayload, normalizeEventPayload } from './eventPayload';

describe('normalizeEventPayload', () => {
  test('normalizes browser/Tauri-style nested payload envelope', () => {
    const payload = normalizeEventPayload<{ value: number }>({
      channel: 'xmpp.message.received',
      payload: {
        type: 'messageReceived',
        data: { value: 1 },
      },
    });

    expect(payload).not.toBeNull();
    expect(payload?.type).toBe('messageReceived');
    expect(payload?.data?.value).toBe(1);
  });

  test('normalizes direct payload envelope', () => {
    const payload = normalizeEventPayload<{ value: string }>({
      type: 'connectionEstablished',
      data: { value: 'ok' },
    });

    expect(payload).not.toBeNull();
    expect(payload?.type).toBe('connectionEstablished');
    expect(payload?.data?.value).toBe('ok');
  });
});

describe('extractMessageFromEventPayload', () => {
  test('extracts message from nested payload envelope', () => {
    const message = extractMessageFromEventPayload<{ id: string }>({
      channel: 'xmpp.message.received',
      payload: {
        type: 'messageReceived',
        data: {
          message: { id: 'msg-1' },
        },
      },
    });

    expect(message?.id).toBe('msg-1');
  });

  test('extracts message from direct payload envelope', () => {
    const message = extractMessageFromEventPayload<{ id: string }>({
      type: 'messageReceived',
      data: {
        message: { id: 'msg-2' },
      },
    });

    expect(message?.id).toBe('msg-2');
  });
});
