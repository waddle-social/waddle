import type { WaddleSession } from "../../src/lib/server-auth";

export const GATE_ZERO_SCOPE = {
  deploymentEnvironment: "production",
  cluster: "waddle-cloud",
  namespace: "waddle",
  sourceId: "waddle-chat",
  release: "0123456789abcdef0123456789abcdef01234567",
};

export function session(partial: Partial<WaddleSession> = {}): WaddleSession {
  return {
    username: "alice",
    jid: "alice@example.com/desktop",
    session_id: "tok",
    xmpp_websocket_url: "wss://example.com/ws",
    ...partial,
  } as WaddleSession;
}

export function createStorageMock() {
  const values = new Map<string, string>();
  return {
    getItem(key: string) {
      return values.has(key) ? values.get(key)! : null;
    },
    setItem(key: string, value: string) {
      values.set(key, value);
    },
    removeItem(key: string) {
      values.delete(key);
    },
    clear() {
      values.clear();
    },
  };
}

export function createFaroStub() {
  const events: Array<{ name: string; attributes?: Record<string, string> }> = [];
  const measurements: Array<{
    type: string;
    values: Record<string, number>;
    context?: Record<string, string>;
  }> = [];
  const errors: Array<{
    error: Error;
    options?: { type?: string; context?: Record<string, string> };
  }> = [];
  return {
    events,
    measurements,
    errors,
    api: {
      pushEvent: (name: string, attributes?: Record<string, string>) => {
        events.push({ name, attributes });
      },
      pushMeasurement: (payload: {
        type: string;
        values: Record<string, number>;
        context?: Record<string, string>;
      }, options?: { context?: Record<string, string> }) => {
        measurements.push({ ...payload, context: options?.context });
      },
      pushError: (
        error: Error,
        options?: { type?: string; context?: Record<string, string> },
      ) => {
        errors.push({ error, options });
      },
    },
  };
}
