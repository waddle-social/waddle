import type { APIRoute } from 'astro';
import { drizzle } from 'drizzle-orm/d1';
import * as schema from '../../../lib/db/schema';
import { eq } from 'drizzle-orm';

export const POST: APIRoute = async ({ locals, cookies, redirect }) => {
  const sessionToken = cookies.get('session')?.value;
  
  if (sessionToken) {
    const drizzleDb = drizzle(locals.runtime.env.DB, { schema });
    
    // Delete session from database
    await drizzleDb.delete(schema.sessions).where(eq(schema.sessions.token, sessionToken));
    
    // Delete session cookie
    cookies.delete('session', { path: '/' });
  }
  
  return redirect('/');
};