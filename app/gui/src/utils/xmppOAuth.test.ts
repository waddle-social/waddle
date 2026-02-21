import { describe, expect, test } from 'bun:test';
import {
  XMPP_OAUTH_PENDING_FLOW_KEY,
  beginXmppOAuthFlow,
  completeXmppOAuthFlow,
  consumePendingXmppOAuthFlow,
  createPkceCodeChallenge,
  createPkceVerifier,
  discoverXmppOAuthMetadata,
  extractXmppDomain,
  type PendingXmppOAuthFlow,
  type XmppOAuthMetadata,
} from './xmppOAuth';

class MemoryStorage {
  private readonly values = new Map<string, string>();

  getItem(key: string): string | null {
    return this.values.get(key) ?? null;
  }

  setItem(key: string, value: string): void {
    this.values.set(key, value);
  }

  removeItem(key: string): void {
    this.values.delete(key);
  }
}

function jsonResponse(payload: unknown, status = 200): Response {
  return new Response(JSON.stringify(payload), {
    status,
    headers: { 'Content-Type': 'application/json' },
  });
}

describe('extractXmppDomain', () => {
  test('parses domain from a bare domain', () => {
    expect(extractXmppDomain('example.com')).toBe('example.com');
  });

  test('parses domain from full JID', () => {
    expect(extractXmppDomain('alice@example.com/mobile')).toBe('example.com');
  });
});

describe('PKCE helpers', () => {
  test('creates URL-safe verifier and challenge', async () => {
    const verifier = createPkceVerifier();
    const challenge = await createPkceCodeChallenge(verifier);

    expect(verifier.length).toBeGreaterThan(30);
    expect(challenge.length).toBeGreaterThan(30);
    expect(verifier).not.toContain('=');
    expect(challenge).not.toContain('=');
  });
});

describe('beginXmppOAuthFlow', () => {
  test('builds authorize URL and persists pending flow', async () => {
    const storage = new MemoryStorage();
    const metadata: XmppOAuthMetadata = {
      issuer: 'https://auth.example.com',
      authorization_endpoint: 'https://auth.example.com/api/auth/xmpp/authorize',
      token_endpoint: 'https://auth.example.com/api/auth/xmpp/token',
    };

    const fetchImpl = async (input: RequestInfo | URL): Promise<Response> => {
      const url = input instanceof URL ? input : new URL(String(input));
      if (url.pathname === '/.well-known/oauth-authorization-server') {
        return jsonResponse(metadata);
      }
      throw new Error(`Unexpected request: ${url.toString()}`);
    };

    const result = await beginXmppOAuthFlow({
      jidOrDomain: 'alice@example.com',
      providerId: 'github',
      endpoint: 'wss://xmpp.example.com/xmpp-websocket',
      redirectUri: 'https://app.example.com/oauth/xmpp/callback',
      callbackBaseUrl: 'https://app.example.com',
      fetchImpl,
      storage,
    });

    const authorizeUrl = new URL(result.authorizeUrl);
    expect(authorizeUrl.origin).toBe('https://auth.example.com');
    expect(authorizeUrl.pathname).toBe('/api/auth/xmpp/authorize');
    expect(authorizeUrl.searchParams.get('provider')).toBe('github');
    expect(authorizeUrl.searchParams.get('response_type')).toBe('code');
    expect(authorizeUrl.searchParams.get('scope')).toBe('xmpp');

    const persisted = storage.getItem(XMPP_OAUTH_PENDING_FLOW_KEY);
    expect(persisted).not.toBeNull();
    const parsed = JSON.parse(persisted ?? '{}') as PendingXmppOAuthFlow;
    expect(parsed.domain).toBe('example.com');
    expect(parsed.endpoint).toBe('wss://xmpp.example.com/xmpp-websocket');
    expect(parsed.tokenEndpoint).toBe('https://auth.example.com/api/auth/xmpp/token');
  });
});

describe('discoverXmppOAuthMetadata', () => {
  test('uses explicit server URL when provided', async () => {
    const metadata: XmppOAuthMetadata = {
      issuer: 'http://localhost:3000',
      authorization_endpoint: 'http://localhost:3000/api/auth/xmpp/authorize',
      token_endpoint: 'http://localhost:3000/api/auth/xmpp/token',
    };

    const fetchImpl = async (input: RequestInfo | URL): Promise<Response> => {
      const url = input instanceof URL ? input : new URL(String(input));
      expect(url.origin).toBe('http://localhost:3000');
      expect(url.pathname).toBe('/.well-known/oauth-authorization-server');
      return jsonResponse(metadata);
    };

    const discovered = await discoverXmppOAuthMetadata('http://localhost:3000', fetchImpl);
    expect(discovered.domain).toBe('localhost');
    expect(discovered.metadata.issuer).toBe('http://localhost:3000');
  });
});

describe('completeXmppOAuthFlow', () => {
  test('exchanges code, resolves session, and builds final JID', async () => {
    const storage = new MemoryStorage();
    const flow: PendingXmppOAuthFlow = {
      state: 'state-123',
      codeVerifier: 'verifier-123',
      redirectUri: 'https://app.example.com/oauth/xmpp/callback',
      domain: 'example.com',
      endpoint: 'wss://example.com/xmpp-websocket',
      providerId: 'github',
      sessionEndpoint: 'https://auth.example.com/api/auth/session',
      tokenEndpoint: 'https://auth.example.com/api/auth/xmpp/token',
      createdAt: Date.now(),
    };
    storage.setItem(XMPP_OAUTH_PENDING_FLOW_KEY, JSON.stringify(flow));

    const fetchImpl = async (input: RequestInfo | URL, init?: RequestInit): Promise<Response> => {
      const url = input instanceof URL ? input : new URL(String(input));
      if (url.pathname === '/api/auth/xmpp/token') {
        expect(init?.method).toBe('POST');
        const body = init?.body;
        expect(body instanceof URLSearchParams).toBe(true);
        if (body instanceof URLSearchParams) {
          expect(body.get('code')).toBe('auth-code-1');
          expect(body.get('code_verifier')).toBe('verifier-123');
        }
        return jsonResponse({
          access_token: 'session-abc',
          token_type: 'Bearer',
          expires_in: 3600,
          scope: 'xmpp',
        });
      }

      if (url.pathname === '/api/auth/session') {
        expect(url.searchParams.get('session_id')).toBe('session-abc');
        return jsonResponse({
          session_id: 'session-abc',
          user_id: 'user-1',
          username: 'alice',
          xmpp_localpart: 'alice',
          is_expired: false,
          expires_at: null,
        });
      }

      throw new Error(`Unexpected request: ${url.toString()}`);
    };

    const result = await completeXmppOAuthFlow({
      code: 'auth-code-1',
      state: 'state-123',
      fetchImpl,
      storage,
    });

    expect(result.token.access_token).toBe('session-abc');
    expect(result.session.username).toBe('alice');
    expect(result.jid).toBe('alice@example.com');
    expect(storage.getItem(XMPP_OAUTH_PENDING_FLOW_KEY)).toBeNull();
  });

  test('rejects callback if the state is invalid', () => {
    const storage = new MemoryStorage();
    const flow: PendingXmppOAuthFlow = {
      state: 'expected-state',
      codeVerifier: 'verifier-123',
      redirectUri: 'https://app.example.com/oauth/xmpp/callback',
      domain: 'example.com',
      endpoint: 'wss://example.com/xmpp-websocket',
      providerId: null,
      sessionEndpoint: 'https://auth.example.com/api/auth/session',
      tokenEndpoint: 'https://auth.example.com/api/auth/xmpp/token',
      createdAt: Date.now(),
    };
    storage.setItem(XMPP_OAUTH_PENDING_FLOW_KEY, JSON.stringify(flow));

    expect(() => consumePendingXmppOAuthFlow('wrong-state', storage)).toThrow(
      'OAuth state mismatch. Please retry sign-in.',
    );
  });
});
