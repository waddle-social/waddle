import { discoverWebSocketEndpoint } from './discover';

export const XMPP_OAUTH_PENDING_FLOW_KEY = 'waddle:auth:xmpp-oauth:pending';

const FLOW_MAX_AGE_MS = 10 * 60 * 1000;
const DEFAULT_CALLBACK_PATH = '/oauth/xmpp/callback';

type FetchLike = (input: RequestInfo | URL, init?: RequestInit) => Promise<Response>;

interface StorageLike {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
  removeItem(key: string): void;
}

export interface XmppOAuthProvider {
  id: string;
  display_name: string;
  kind: string;
}

export interface XmppOAuthMetadata {
  issuer: string;
  authorization_endpoint: string;
  token_endpoint: string;
}

export interface XmppOAuthTokenResponse {
  access_token: string;
  token_type: string;
  expires_in: number;
  scope: string;
}

export interface XmppOAuthSessionResponse {
  session_id: string;
  user_id: string;
  username: string;
  xmpp_localpart: string;
  is_expired: boolean;
  expires_at: string | null;
}

export interface PendingXmppOAuthFlow {
  state: string;
  codeVerifier: string;
  redirectUri: string;
  domain: string;
  endpoint: string;
  providerId: string | null;
  sessionEndpoint: string;
  tokenEndpoint: string;
  createdAt: number;
}

export interface BeginXmppOAuthFlowInput {
  jidOrDomain: string;
  providerId?: string;
  endpoint?: string;
  redirectUri?: string;
  callbackBaseUrl?: string;
  fetchImpl?: FetchLike;
  storage?: StorageLike;
}

export interface CompleteXmppOAuthFlowInput {
  code: string;
  state: string;
  fetchImpl?: FetchLike;
  storage?: StorageLike;
}

interface OAuthErrorResponse {
  error?: string;
  message?: string;
}

function getFetch(fetchImpl?: FetchLike): FetchLike {
  if (fetchImpl) return fetchImpl;
  if (typeof fetch === 'function') return fetch.bind(globalThis);
  throw new Error('Fetch API is not available in this runtime');
}

function getStorage(storage?: StorageLike): StorageLike {
  if (storage) return storage;
  if (typeof window !== 'undefined' && window.localStorage) {
    return window.localStorage;
  }
  throw new Error('Local storage is not available in this runtime');
}

function getBaseUrl(baseUrl?: string): string {
  if (baseUrl) return baseUrl;
  if (typeof window !== 'undefined' && window.location.origin) {
    return window.location.origin;
  }
  throw new Error('Unable to resolve base URL');
}

function resolveOAuthServerBase(input: string): { baseUrl: string; domain: string } {
  const trimmed = input.trim();
  if (!trimmed) {
    throw new Error('Server URL or domain is required');
  }

  if (trimmed.includes('://')) {
    let parsed: URL;
    try {
      parsed = new URL(trimmed);
    } catch {
      throw new Error('Invalid server URL');
    }

    if (parsed.protocol !== 'https:' && parsed.protocol !== 'http:') {
      throw new Error('Server URL must start with http:// or https://');
    }

    return {
      baseUrl: parsed.origin,
      domain: parsed.hostname,
    };
  }

  const domain = extractXmppDomain(trimmed);
  return {
    baseUrl: `https://${domain}`,
    domain,
  };
}

async function parseJsonOrThrow<T>(response: Response, fallback: string): Promise<T> {
  try {
    return (await response.json()) as T;
  } catch {
    throw new Error(fallback);
  }
}

function toBase64Url(bytes: Uint8Array): string {
  if (typeof btoa !== 'function') {
    throw new Error('btoa is not available in this runtime');
  }
  const binary = Array.from(bytes, (byte) => String.fromCharCode(byte)).join('');
  return btoa(binary).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/u, '');
}

function randomBytes(length: number): Uint8Array {
  if (!globalThis.crypto?.getRandomValues) {
    throw new Error('Secure random generator is not available');
  }
  const bytes = new Uint8Array(length);
  globalThis.crypto.getRandomValues(bytes);
  return bytes;
}

async function parseErrorResponse(response: Response, fallback: string): Promise<Error> {
  let message = `${fallback} (${response.status})`;
  try {
    const payload = (await response.json()) as OAuthErrorResponse;
    if (typeof payload.message === 'string' && payload.message.trim().length > 0) {
      message = payload.message;
    } else if (typeof payload.error === 'string' && payload.error.trim().length > 0) {
      message = payload.error;
    }
  } catch {
    // no-op: use fallback message
  }
  return new Error(message);
}

export function extractXmppDomain(input: string): string {
  const trimmed = input.trim();
  if (!trimmed) {
    throw new Error('JID or domain is required');
  }

  const bare = trimmed.split('/')[0] || trimmed;

  if (bare.includes('://')) {
    try {
      const parsed = new URL(bare);
      if (parsed.hostname) return parsed.hostname;
    } catch {
      // Continue to JID/domain parsing below
    }
  }

  if (bare.includes('@')) {
    const [, domain] = bare.split('@', 2);
    if (!domain || domain.trim().length === 0) {
      throw new Error('Invalid JID — expected user@domain');
    }
    return domain.trim().toLowerCase();
  }

  return bare.toLowerCase();
}

export function buildXmppJid(localpart: string, domain: string): string {
  const cleanLocalpart = localpart.trim();
  const cleanDomain = domain.trim();
  if (!cleanLocalpart || !cleanDomain) {
    throw new Error('Unable to build JID from OAuth session');
  }
  return `${cleanLocalpart}@${cleanDomain}`;
}

export function createPkceVerifier(): string {
  return toBase64Url(randomBytes(32));
}

export async function createPkceCodeChallenge(verifier: string): Promise<string> {
  if (!globalThis.crypto?.subtle) {
    throw new Error('WebCrypto subtle API is not available');
  }
  const encoded = new TextEncoder().encode(verifier);
  const digest = await globalThis.crypto.subtle.digest('SHA-256', encoded);
  return toBase64Url(new Uint8Array(digest));
}

export function createOAuthState(): string {
  return toBase64Url(randomBytes(24));
}

export async function discoverXmppOAuthMetadata(
  serverUrlOrDomain: string,
  fetchImpl?: FetchLike,
): Promise<{ domain: string; metadata: XmppOAuthMetadata }> {
  const { baseUrl, domain } = resolveOAuthServerBase(serverUrlOrDomain);
  const requestUrl = new URL('/.well-known/oauth-authorization-server', baseUrl);
  const doFetch = getFetch(fetchImpl);

  const response = await doFetch(requestUrl, {
    method: 'GET',
    credentials: 'omit',
  });

  if (!response.ok) {
    throw await parseErrorResponse(response, 'Failed to discover OAuth metadata');
  }

  const payload = await parseJsonOrThrow<Partial<XmppOAuthMetadata>>(
    response,
    `OAuth metadata for ${domain} is not valid JSON`,
  );
  if (
    typeof payload.issuer !== 'string'
    || typeof payload.authorization_endpoint !== 'string'
    || typeof payload.token_endpoint !== 'string'
  ) {
    throw new Error('OAuth metadata response is invalid');
  }

  return { domain, metadata: payload as XmppOAuthMetadata };
}

export async function fetchXmppOAuthProviders(
  serverUrlOrDomain: string,
  fetchImpl?: FetchLike,
): Promise<XmppOAuthProvider[]> {
  const { metadata } = await discoverXmppOAuthMetadata(serverUrlOrDomain, fetchImpl);
  const requestUrl = new URL('/api/auth/providers', metadata.issuer);
  const doFetch = getFetch(fetchImpl);

  const response = await doFetch(requestUrl, {
    method: 'GET',
    credentials: 'omit',
  });

  if (!response.ok) {
    throw await parseErrorResponse(response, 'Failed to load OAuth providers');
  }

  const payload = await parseJsonOrThrow<unknown>(
    response,
    'OAuth providers response is not valid JSON',
  );
  if (!Array.isArray(payload)) {
    throw new Error('OAuth providers response is invalid');
  }

  return payload
    .filter((provider): provider is XmppOAuthProvider => {
      if (!provider || typeof provider !== 'object') return false;
      const candidate = provider as Partial<XmppOAuthProvider>;
      return (
        typeof candidate.id === 'string'
        && typeof candidate.display_name === 'string'
        && typeof candidate.kind === 'string'
      );
    });
}

export function buildXmppOAuthAuthorizeUrl(input: {
  authorizationEndpoint: string;
  redirectUri: string;
  state: string;
  codeChallenge: string;
  providerId?: string;
}): string {
  const authorizeUrl = new URL(input.authorizationEndpoint);
  authorizeUrl.searchParams.set('response_type', 'code');
  authorizeUrl.searchParams.set('redirect_uri', input.redirectUri);
  authorizeUrl.searchParams.set('scope', 'xmpp');
  authorizeUrl.searchParams.set('state', input.state);
  authorizeUrl.searchParams.set('code_challenge', input.codeChallenge);
  authorizeUrl.searchParams.set('code_challenge_method', 'S256');
  if (input.providerId && input.providerId.trim().length > 0) {
    authorizeUrl.searchParams.set('provider', input.providerId);
  }
  return authorizeUrl.toString();
}

export function storePendingXmppOAuthFlow(
  flow: PendingXmppOAuthFlow,
  storage?: StorageLike,
): void {
  getStorage(storage).setItem(XMPP_OAUTH_PENDING_FLOW_KEY, JSON.stringify(flow));
}

export function consumePendingXmppOAuthFlow(
  expectedState: string,
  storage?: StorageLike,
): PendingXmppOAuthFlow {
  const store = getStorage(storage);
  const raw = store.getItem(XMPP_OAUTH_PENDING_FLOW_KEY);
  store.removeItem(XMPP_OAUTH_PENDING_FLOW_KEY);

  if (!raw) {
    throw new Error('Missing OAuth flow state. Please retry sign-in.');
  }

  let parsed: PendingXmppOAuthFlow;
  try {
    parsed = JSON.parse(raw) as PendingXmppOAuthFlow;
  } catch {
    throw new Error('Stored OAuth flow is invalid. Please retry sign-in.');
  }

  if (parsed.state !== expectedState) {
    throw new Error('OAuth state mismatch. Please retry sign-in.');
  }

  if (
    typeof parsed.createdAt !== 'number'
    || parsed.createdAt + FLOW_MAX_AGE_MS < Date.now()
  ) {
    throw new Error('OAuth flow has expired. Please retry sign-in.');
  }

  return parsed;
}

export async function exchangeXmppOAuthCode(input: {
  tokenEndpoint: string;
  code: string;
  redirectUri: string;
  codeVerifier: string;
  fetchImpl?: FetchLike;
}): Promise<XmppOAuthTokenResponse> {
  const doFetch = getFetch(input.fetchImpl);
  const body = new URLSearchParams({
    grant_type: 'authorization_code',
    code: input.code,
    redirect_uri: input.redirectUri,
    code_verifier: input.codeVerifier,
  });

  const response = await doFetch(input.tokenEndpoint, {
    method: 'POST',
    headers: {
      'Content-Type': 'application/x-www-form-urlencoded',
    },
    body,
    credentials: 'omit',
  });

  if (!response.ok) {
    throw await parseErrorResponse(response, 'OAuth token exchange failed');
  }

  const payload = (await response.json()) as Partial<XmppOAuthTokenResponse>;
  if (
    typeof payload.access_token !== 'string'
    || typeof payload.token_type !== 'string'
    || typeof payload.scope !== 'string'
    || typeof payload.expires_in !== 'number'
  ) {
    throw new Error('OAuth token response is invalid');
  }

  return payload as XmppOAuthTokenResponse;
}

export async function fetchXmppOAuthSession(input: {
  sessionEndpoint: string;
  sessionId: string;
  fetchImpl?: FetchLike;
}): Promise<XmppOAuthSessionResponse> {
  const doFetch = getFetch(input.fetchImpl);
  const endpoint = new URL(input.sessionEndpoint);
  endpoint.searchParams.set('session_id', input.sessionId);

  const response = await doFetch(endpoint, {
    method: 'GET',
    credentials: 'omit',
  });

  if (!response.ok) {
    throw await parseErrorResponse(response, 'Failed to resolve OAuth session');
  }

  const payload = (await response.json()) as Partial<XmppOAuthSessionResponse>;
  if (
    typeof payload.session_id !== 'string'
    || typeof payload.user_id !== 'string'
    || typeof payload.username !== 'string'
    || typeof payload.xmpp_localpart !== 'string'
    || typeof payload.is_expired !== 'boolean'
  ) {
    throw new Error('OAuth session response is invalid');
  }

  return {
    session_id: payload.session_id,
    user_id: payload.user_id,
    username: payload.username,
    xmpp_localpart: payload.xmpp_localpart,
    is_expired: payload.is_expired,
    expires_at: payload.expires_at ?? null,
  };
}

export async function beginXmppOAuthFlow(
  input: BeginXmppOAuthFlowInput,
): Promise<{ authorizeUrl: string; flow: PendingXmppOAuthFlow }> {
  const { domain, metadata } = await discoverXmppOAuthMetadata(input.jidOrDomain, input.fetchImpl);
  const resolvedEndpoint = input.endpoint?.trim() || await discoverWebSocketEndpoint(domain);
  const resolvedBaseUrl = getBaseUrl(input.callbackBaseUrl);
  const resolvedRedirectUri = input.redirectUri
    ?? new URL(DEFAULT_CALLBACK_PATH, resolvedBaseUrl).toString();
  const codeVerifier = createPkceVerifier();
  const state = createOAuthState();
  const codeChallenge = await createPkceCodeChallenge(codeVerifier);

  const flow: PendingXmppOAuthFlow = {
    state,
    codeVerifier,
    redirectUri: resolvedRedirectUri,
    domain,
    endpoint: resolvedEndpoint,
    providerId: input.providerId ?? null,
    sessionEndpoint: new URL('/api/auth/session', metadata.issuer).toString(),
    tokenEndpoint: metadata.token_endpoint,
    createdAt: Date.now(),
  };

  storePendingXmppOAuthFlow(flow, input.storage);

  const authorizeUrl = buildXmppOAuthAuthorizeUrl({
    authorizationEndpoint: metadata.authorization_endpoint,
    redirectUri: resolvedRedirectUri,
    state,
    codeChallenge,
    providerId: input.providerId,
  });

  return { authorizeUrl, flow };
}

export async function completeXmppOAuthFlow(input: CompleteXmppOAuthFlowInput): Promise<{
  flow: PendingXmppOAuthFlow;
  token: XmppOAuthTokenResponse;
  session: XmppOAuthSessionResponse;
  jid: string;
}> {
  const flow = consumePendingXmppOAuthFlow(input.state, input.storage);
  const token = await exchangeXmppOAuthCode({
    tokenEndpoint: flow.tokenEndpoint,
    code: input.code,
    redirectUri: flow.redirectUri,
    codeVerifier: flow.codeVerifier,
    fetchImpl: input.fetchImpl,
  });

  const session = await fetchXmppOAuthSession({
    sessionEndpoint: flow.sessionEndpoint,
    sessionId: token.access_token,
    fetchImpl: input.fetchImpl,
  });

  if (session.is_expired) {
    throw new Error('OAuth session has already expired. Please retry sign-in.');
  }

  return {
    flow,
    token,
    session,
    jid: buildXmppJid(session.xmpp_localpart, flow.domain),
  };
}
