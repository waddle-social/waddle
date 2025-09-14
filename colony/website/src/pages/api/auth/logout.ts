import type { APIRoute } from 'astro';
import { deleteSession } from '../../../lib/auth/workos';

export const POST: APIRoute = async ({ locals, cookies, redirect }) => {
  const sessionId = cookies.get('colony_session')?.value;
  
  if (sessionId) {
    await deleteSession(locals.runtime.env.AUTH_DB, sessionId);
    cookies.delete('colony_session', { path: '/' });
  }
  
  return redirect('/');
};