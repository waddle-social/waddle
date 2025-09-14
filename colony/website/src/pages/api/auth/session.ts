import type { APIRoute } from 'astro';
import { drizzle } from 'drizzle-orm/d1';
import * as schema from '../../../lib/db/schema';
import { eq } from 'drizzle-orm';

export const GET: APIRoute = async ({ locals, cookies }) => {
  try {
    const sessionToken = cookies.get('session')?.value;
    
    if (!sessionToken) {
      return new Response(JSON.stringify({ authenticated: false }), {
        status: 200,
        headers: { 'Content-Type': 'application/json' },
      });
    }
    
    const drizzleDb = drizzle(locals.runtime.env.DB, { schema });
    
    // Get session with user data
    const result = await drizzleDb
      .select({
        session: schema.sessions,
        user: schema.users,
      })
      .from(schema.sessions)
      .innerJoin(schema.users, eq(schema.sessions.userId, schema.users.id))
      .where(eq(schema.sessions.token, sessionToken))
      .limit(1);
    
    if (result.length === 0) {
      return new Response(JSON.stringify({ authenticated: false }), {
        status: 200,
        headers: { 'Content-Type': 'application/json' },
      });
    }
    
    const { session, user } = result[0];
    
    // Check if session is expired
    if (session.expiresAt < new Date()) {
      // Delete expired session
      await drizzleDb.delete(schema.sessions).where(eq(schema.sessions.id, session.id));
      cookies.delete('session');
      
      return new Response(JSON.stringify({ authenticated: false }), {
        status: 200,
        headers: { 'Content-Type': 'application/json' },
      });
    }
    
    return new Response(JSON.stringify({
      authenticated: true,
      user: {
        id: user.id,
        handle: user.handle,
        name: user.name,
        image: user.image,
        did: user.did,
      },
    }), {
      status: 200,
      headers: { 'Content-Type': 'application/json' },
    });
  } catch (error) {
    console.error('Session check error:', error);
    return new Response(JSON.stringify({ authenticated: false }), {
      status: 200,
      headers: { 'Content-Type': 'application/json' },
    });
  }
};