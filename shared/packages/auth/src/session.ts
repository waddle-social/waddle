export interface AuthSession {
  userId: string;
  email: string;
  organizationId?: string;
  expiresAt: number;
}

export interface SessionStore {
  create(session: AuthSession): Promise<string>;
  get(sessionId: string): Promise<AuthSession | null>;
  delete(sessionId: string): Promise<void>;
}

export class KVSessionStore implements SessionStore {
  constructor(private kv: KVNamespace) {}

  async create(session: AuthSession): Promise<string> {
    const sessionId = crypto.randomUUID();
    await this.kv.put(
      `session:${sessionId}`,
      JSON.stringify(session),
      {
        expirationTtl: 86400 * 7, // 7 days
      }
    );
    return sessionId;
  }

  async get(sessionId: string): Promise<AuthSession | null> {
    const data = await this.kv.get(`session:${sessionId}`);
    if (!data) return null;
    
    const session = JSON.parse(data) as AuthSession;
    if (session.expiresAt < Date.now()) {
      await this.kv.delete(`session:${sessionId}`);
      return null;
    }
    
    return session;
  }

  async delete(sessionId: string): Promise<void> {
    await this.kv.delete(`session:${sessionId}`);
  }
}