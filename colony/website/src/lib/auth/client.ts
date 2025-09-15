import { createAuthClient } from '@better-auth/client';
import { genericOAuthClient } from '@better-auth/client/plugins';

export const authClient = createAuthClient({
  baseURL: typeof window !== 'undefined' ? window.location.origin : '',
  plugins: [genericOAuthClient()],
});