import type { APIRoute } from 'astro';
import { createBlueskyOAuthClient } from '../../../../lib/auth/bluesky-oauth';
import { createAuth } from '../../../../lib/auth/better-auth';
import type { BlueskyJWK } from '../../../../lib/auth/keys';
import { drizzle } from 'drizzle-orm/d1';
import * as schema from '../../../../lib/db/schema';
import { eq } from 'drizzle-orm';

export const GET: APIRoute = async ({ url, locals, cookies, redirect }) => {
  try {
    const code = url.searchParams.get('code');
    const state = url.searchParams.get('state');
    const storedState = cookies.get('oauth_state')?.value;
    
    // Verify state for CSRF protection
    if (!state || state !== storedState) {
      return new Response('Invalid state', { status: 400 });
    }
    
    if (!code) {
      return new Response('Missing authorization code', { status: 400 });
    }
    
    const privateKeyStr = await locals.runtime.env.ATPROTO_PRIVATE_KEY.get();
    if (!privateKeyStr) {
      return new Response('OAuth not configured', { status: 500 });
    }
    
    const privateKey: BlueskyJWK = JSON.parse(privateKeyStr);
    const baseUrl = url.origin;
    
    const client = await createBlueskyOAuthClient({
      baseUrl,
      privateKey,
      db: locals.runtime.env.DB,
    });
    
    // Exchange code for tokens
    const searchParams = new URLSearchParams(url.search);
    const { session } = await client.callback(searchParams);
    
    // Get user info from Bluesky
    const agent = new (await import('@atproto/api')).Agent(session);
    const profile = await agent.getProfile({ actor: session.did });
    
    // Create or update user in BetterAuth database
    const drizzleDb = drizzle(locals.runtime.env.DB, { schema });
    
    // Check if user exists
    const existingUser = await drizzleDb
      .select()
      .from(schema.users)
      .where(eq(schema.users.did, session.did))
      .limit(1);
    
    let userId: string;
    
    if (existingUser.length > 0) {
      userId = existingUser[0].id;
      // Update user info
      await drizzleDb
        .update(schema.users)
        .set({
          handle: profile.data.handle,
          name: profile.data.displayName,
          image: profile.data.avatar,
          updatedAt: new Date(),
        })
        .where(eq(schema.users.id, userId));
    } else {
      // Create new user
      userId = crypto.randomUUID();
      await drizzleDb.insert(schema.users).values({
        id: userId,
        did: session.did,
        handle: profile.data.handle,
        name: profile.data.displayName,
        image: profile.data.avatar,
        createdAt: new Date(),
        updatedAt: new Date(),
      });
    }
    
    // Create session using BetterAuth
    const auth = createAuth(locals.runtime.env.DB, locals.runtime.env);
    const sessionToken = crypto.randomUUID();
    
    await drizzleDb.insert(schema.sessions).values({
      id: crypto.randomUUID(),
      userId,
      token: sessionToken,
      expiresAt: new Date(Date.now() + 7 * 24 * 60 * 60 * 1000), // 7 days
      createdAt: new Date(),
      updatedAt: new Date(),
    });
    
    // Set session cookie
    cookies.set('session', sessionToken, {
      httpOnly: true,
      secure: url.protocol === 'https:',
      sameSite: 'lax',
      maxAge: 60 * 60 * 24 * 7, // 7 days
      path: '/',
    });
    
    // Clear OAuth state
    cookies.delete('oauth_state');
    
    // Redirect to dashboard or home
    return redirect('/dashboard');
  } catch (error) {
    console.error('OAuth callback error:', error);
    return redirect('/?error=auth_failed');
  }
};