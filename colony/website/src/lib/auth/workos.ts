import { WorkOS } from '@workos-inc/node';

export function createWorkOSClient(apiKey: string) {
  return new WorkOS(apiKey);
}

export interface AuthSession {
  userId: string;
  email: string;
  organizationId?: string;
  expiresAt: number;
}

export async function createSession(
  kv: KVNamespace,
  session: AuthSession
): Promise<string> {
  const sessionId = crypto.randomUUID();
  await kv.put(
    `session:${sessionId}`,
    JSON.stringify(session),
    {
      expirationTtl: 86400 * 7, // 7 days
    }
  );
  return sessionId;
}

export async function getSession(
  kv: KVNamespace,
  sessionId: string
): Promise<AuthSession | null> {
  const data = await kv.get(`session:${sessionId}`);
  if (!data) return null;
  
  const session = JSON.parse(data) as AuthSession;
  if (session.expiresAt < Date.now()) {
    await kv.delete(`session:${sessionId}`);
    return null;
  }
  
  return session;
}

export async function deleteSession(
  kv: KVNamespace,
  sessionId: string
): Promise<void> {
  await kv.delete(`session:${sessionId}`);
}