import type { APIRoute } from 'astro';
import { createWorkOSClient } from '../../../lib/auth/workos';

export const GET: APIRoute = async ({ locals, url, redirect }) => {
  const workos = createWorkOSClient(locals.runtime.env.WORKOS_API_KEY);
  
  const returnUrl = url.searchParams.get('returnUrl') || '/';
  const app = url.searchParams.get('app') || 'waddle';
  
  const authorizationUrl = workos.userManagement.getAuthorizationUrl({
    provider: 'authkit',
    clientId: locals.runtime.env.WORKOS_CLIENT_ID,
    redirectUri: `${url.origin}/api/auth/callback`,
    state: JSON.stringify({ returnUrl, app }),
  });
  
  return redirect(authorizationUrl);
};