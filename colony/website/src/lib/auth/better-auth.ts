import { betterAuth } from 'better-auth';
import { genericOAuth } from "better-auth/plugins";
import { drizzleAdapter } from 'better-auth/adapters/drizzle';
import { drizzle } from 'drizzle-orm/d1';
import * as schema from '../db/schema';
import type { D1Database } from '@cloudflare/workers-types';
import { WorkerOAuthClient } from "./atproto-oauth-worker";
import { resolveHandle } from "./handle-resolver";
import type { BlueskyJWK } from "./keys";

export async function createAuth(db: D1Database, env: any, request?: Request) {
  const drizzleDb = drizzle(db, { schema });

		// Helper function to get the origin
		const getOrigin = (req?: Request) => {
			const r = req || request;
			if (r) {
				const url = new URL(r.url);
				return url.origin;
			}
			return env.SITE_URL || "http://localhost:4321";
		};

		// Helper function to get the private key
		const getPrivateKey = async (): Promise<BlueskyJWK> => {
			let privateKeyStr: string | undefined;
			const secretBinding = env.ATPROTO_PRIVATE_KEY;
			if (secretBinding) {
				if (typeof secretBinding.get === "function") {
					privateKeyStr = await secretBinding.get();
				} else if (typeof secretBinding === "string") {
					privateKeyStr = secretBinding;
				}
			}
			if (!privateKeyStr && env.BLUESKY_PRIVATE_KEY) {
				privateKeyStr = env.BLUESKY_PRIVATE_KEY;
			}
			if (!privateKeyStr) {
				throw new Error("OAuth private key not configured");
			}
			return JSON.parse(privateKeyStr);
		};

		// Resolve Better Auth secret from env or Secret Store
		let resolvedSecret: string | undefined;
		const rawSecret = (env as any).BETTER_AUTH_SECRET;
		if (rawSecret && typeof rawSecret.get === "function") {
			resolvedSecret = await rawSecret.get();
		} else if (typeof rawSecret === "string") {
			resolvedSecret = rawSecret;
		}

		return betterAuth({
			secret: resolvedSecret,
			database: drizzleAdapter(drizzleDb, {
				provider: "sqlite",
				schema: {
					user: schema.users,
					session: schema.sessions,
					account: schema.accounts,
					verification: schema.verifications,
				},
			}),
			emailAndPassword: {
				enabled: false, // We only use ATProto OAuth
			},
			plugins: [
				genericOAuth({
					config: [
						{
							providerId: "atproto",
							clientId: `${getOrigin()}/client-metadata.json`,
							clientSecret: "not-used", // ATProto uses private_key_jwt instead
							authorizationUrl: "https://bsky.social/oauth/authorize",
							tokenUrl: "https://bsky.social/oauth/token",
							scopes: ["atproto", "transition:email"],
							pkce: true, // ATProto requires PKCE
							// Custom token exchange using private_key_jwt and DPoP
							getAccessToken: async ({ code, codeVerifier, redirectUri }) => {
								const origin = getOrigin();
								const privateKey = await getPrivateKey();

								// Create OAuth client and generate client assertion
								const oauthClient = new WorkerOAuthClient({
									clientId: `${origin}/client-metadata.json`,
									redirectUri: `${origin}/api/auth/oauth2/callback/atproto`,
									privateKey,
								});
								const clientAssertion =
									await oauthClient.createClientAssertion();

								// Create DPoP proof for the token endpoint
								const tokenUrl = "https://bsky.social/oauth/token";
								let { proof: dpopProof, publicKey: dpopPublicKey, keyPair: dpopKeyPair } =
									await oauthClient.createDPoPProof("POST", tokenUrl);

								// Exchange code for tokens with PKCE
								const tokenParams = new URLSearchParams({
									grant_type: "authorization_code",
									code,
									redirect_uri: `${origin}/api/auth/oauth2/callback/atproto`,
									client_id: `${origin}/client-metadata.json`,
									client_assertion_type:
										"urn:ietf:params:oauth:client-assertion-type:jwt-bearer",
									client_assertion: clientAssertion,
									code_verifier: codeVerifier || "", // Include PKCE code verifier from Better Auth
								});

								let tokenRes = await fetch(
									tokenUrl,
									{
										method: "POST",
										headers: {
											"Content-Type": "application/x-www-form-urlencoded",
											"DPoP": dpopProof,
										},
										body: tokenParams.toString(),
									},
								);

								// Check for DPoP-Nonce in response headers
								console.log("Token response status:", tokenRes.status);
								console.log("Token response headers:", Object.fromEntries(tokenRes.headers.entries()));

								// If we get a 401 with a DPoP-Nonce header, retry with the nonce
								if (tokenRes.status === 401 || tokenRes.status === 400) {
									const dpopNonce = tokenRes.headers.get("DPoP-Nonce") || tokenRes.headers.get("dpop-nonce");
									const responseText = await tokenRes.text();
									console.log("Error response:", responseText);

									if (dpopNonce) {
										console.log("Retrying with DPoP nonce:", dpopNonce);
										const retryProof = await oauthClient.createDPoPProof("POST", tokenUrl, dpopNonce, dpopKeyPair);

										tokenRes = await fetch(
											tokenUrl,
											{
												method: "POST",
												headers: {
													"Content-Type": "application/x-www-form-urlencoded",
													"DPoP": retryProof.proof,
												},
												body: tokenParams.toString(),
											},
										);
										console.log("Retry response status:", tokenRes.status);
									} else {
										// Re-create the response so we can read it again
										tokenRes = new Response(responseText, {
											status: tokenRes.status,
											statusText: tokenRes.statusText,
											headers: tokenRes.headers
										});
									}
								}

								if (!tokenRes.ok) {
									const error = await tokenRes.text();
									console.error(
										"Token exchange failed:",
										tokenRes.status,
										error,
									);
									throw new Error(
										"Failed to exchange authorization code for tokens",
									);
								}

								const tokens = await tokenRes.json();

								// Store DPoP public key in the token response for future use
								return {
									accessToken: tokens.access_token,
									refreshToken: tokens.refresh_token,
									accessTokenExpiresAt: tokens.expires_in
										? new Date(Date.now() + tokens.expires_in * 1000)
										: undefined,
									idToken: tokens.id_token,
									tokenType: tokens.token_type || "DPoP", // ATProto uses DPoP tokens
									dpopPublicKey: JSON.stringify(dpopPublicKey), // Store for future API calls
								};
							},
							getUserInfo: async ({ accessToken }) => {
								// ATProto doesn't have a standard userinfo endpoint
								// We would need to extract info from the ID token or make API calls
								// For now, return minimal info
								return {
									id: crypto.randomUUID(),
									name: null,
									email: null,
									image: null,
									emailVerified: false,
								};
							},
							// Override the authorization URL builder to use our client_id format
							createAuthorizationURL: ({ state, scopes, redirectUri, codeChallenge, codeChallengeMethod }) => {
								const origin = getOrigin();
								const params = new URLSearchParams({
									response_type: "code",
									client_id: `${origin}/client-metadata.json`,
									redirect_uri: `${origin}/api/auth/oauth2/callback/atproto`,
									state,
									scope: "atproto transition:email",
								});

								// Add PKCE parameters if provided
								if (codeChallenge) {
									params.set("code_challenge", codeChallenge);
									params.set("code_challenge_method", codeChallengeMethod || "S256");
								}

								// Add login_hint if available from the request
								if (request) {
									const body = request.body;
									if (body && typeof body === 'object' && 'handle' in body) {
										params.set("login_hint", body.handle as string);
									}
								}

								return `https://bsky.social/oauth/authorize?${params.toString()}`;
							},
						},
					],
				}),
			],
			session: {
				expiresIn: 60 * 60 * 24 * 7, // 7 days
				updateAge: 60 * 60 * 24, // 1 day
				cookieCache: {
					enabled: true,
					maxAge: 60 * 5, // 5 minutes
				},
			},
			trustedOrigins: [
				"https://colony.waddle.social",
				"https://colony.preview.waddle.social",
				"http://localhost:4321", // For local development
			],
		});
}

export type Auth = ReturnType<typeof createAuth>;
