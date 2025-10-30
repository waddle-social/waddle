import { COLONY_ALLOWED_REDIRECTS } from 'astro:env/server';

const REDIRECT_COOKIE_NAME = 'colony_redirect_to';
const REDIRECT_MAX_AGE_SECONDS = 600;

const DEFAULT_LOCAL_ORIGINS = [
  'http://localhost:3000',
  'http://localhost:4321',
  'http://localhost:4322',
  'http://localhost:4323',
];

const trim = (value: string) => value.trim();

const normaliseOrigin = (value: string, fallbackOrigin: string): string | null => {
  try {
    return new URL(value, fallbackOrigin).origin;
  } catch {
    return null;
  }
};

const collectAllowedOrigins = (origin: string, env: Record<string, unknown>): Set<string> => {
  const allowed = new Set<string>([origin]);

  for (const local of DEFAULT_LOCAL_ORIGINS) {
    const normalised = normaliseOrigin(local, origin);
    if (normalised) allowed.add(normalised);
  }

  // Only use the value provided via Astro env schema.
  const envValue = COLONY_ALLOWED_REDIRECTS || '';
  if (envValue) {
    envValue
      .split(',')
      .map(trim)
      .filter(Boolean)
      .forEach((entry) => {
        const normalised = normaliseOrigin(entry, origin);
        if (normalised) allowed.add(normalised);
      });
  }

  return allowed;
};

export const extractCookieValue = (cookieHeader: string | null, name: string): string | null => {
  if (!cookieHeader) return null;
  const cookies = cookieHeader.split(';');
  for (const cookie of cookies) {
    const separatorIndex = cookie.indexOf('=');
    if (separatorIndex === -1) continue;
    const rawName = cookie.slice(0, separatorIndex).trim();
    const rawValue = cookie.slice(separatorIndex + 1).trim();
    if (!rawName || !rawValue) continue;
    if (rawName === name) {
      return decodeURIComponent(rawValue);
    }
  }
  return null;
};

export const resolveRedirectTarget = (
  rawTarget: string | null | undefined,
  origin: string,
  env: Record<string, unknown>,
): string | null => {
  if (!rawTarget) return null;
  let targetUrl: URL;
  try {
    targetUrl = new URL(rawTarget, origin);
  } catch {
    return null;
  }

  const allowedOrigins = collectAllowedOrigins(origin, env);
  if (!allowedOrigins.has(targetUrl.origin)) {
    return null;
  }

  return targetUrl.toString();
};

export const buildRedirectCookie = (target: string, origin: string): string => {
  const secure = origin.startsWith('https') ? '; Secure' : '';
  return `${REDIRECT_COOKIE_NAME}=${encodeURIComponent(target)}; Path=/; HttpOnly${secure}; SameSite=Lax; Max-Age=${REDIRECT_MAX_AGE_SECONDS}`;
};

export const clearRedirectCookie = (origin: string): string => {
  const secure = origin.startsWith('https') ? '; Secure' : '';
  return `${REDIRECT_COOKIE_NAME}=; Path=/; Max-Age=0${secure}; SameSite=Lax`;
};

export { REDIRECT_COOKIE_NAME };

export const getAllowedOrigins = (origin: string, env: Record<string, unknown>): string[] => [
  ...collectAllowedOrigins(origin, env),
];
