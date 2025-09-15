import type { APIRoute } from "astro";
import { createAuth } from "../../../../../lib/auth/better-auth";
import { WorkerOAuthClient } from "../../../../../lib/auth/atproto-oauth-worker";
import type { BlueskyJWK } from "../../../../../lib/auth/keys";
import { serializeSignedCookie } from "better-call";

export const GET: APIRoute = async ({ request, url, locals, redirect }) => {
	try {
		// Extract query parameters
		const code = url.searchParams.get("code");
		const state = url.searchParams.get("state");
		const error = url.searchParams.get("error");
		const errorDescription = url.searchParams.get("error_description");

		// Handle errors from the authorization server
		if (error) {
			console.error("OAuth error:", error, errorDescription);
			return redirect(`/api/auth/error?error=${encodeURIComponent(error)}&error_description=${encodeURIComponent(errorDescription || '')}`);
		}

		if (!code || !state) {
			return redirect("/api/auth/error?error=invalid_request&error_description=Missing%20code%20or%20state");
		}

		// Get environment variables
		const env = locals.runtime.env;
		const origin = url.origin;

		// Get the private key for client authentication
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
		const privateKey: BlueskyJWK = JSON.parse(privateKeyStr);

		// Create OAuth client and generate client assertion
		const oauthClient = new WorkerOAuthClient({
			clientId: `${origin}/client-metadata.json`,
			redirectUri: `${origin}/api/auth/oauth2/callback/atproto`,
			privateKey,
		});

		// Generate client assertion for private_key_jwt authentication
		const clientAssertion = await oauthClient.createClientAssertion();

		// Get code verifier from cookie
		const cookieHeader = request.headers.get("cookie");
		let codeVerifier = "";
		if (cookieHeader) {
			console.log("Cookie header:", cookieHeader);
			const cookies = cookieHeader.split(';').map(c => c.trim());
			const verifierCookie = cookies.find(c => c.startsWith('atproto_oauth_verifier='));
			if (verifierCookie) {
				codeVerifier = decodeURIComponent(verifierCookie.split('=')[1]);
				console.log("Found code verifier, length:", codeVerifier.length);
			} else {
				console.log("No verifier cookie found in:", cookies);
			}
		} else {
			console.log("No cookie header found");
		}

		if (!codeVerifier || codeVerifier.length < 43) {
			console.error("Invalid or missing code verifier:", codeVerifier);
			return redirect("/api/auth/error?error=invalid_request&error_description=Missing%20PKCE%20code%20verifier");
		}

		// First, try to make a token request without DPoP to get the nonce
		const tokenUrl = "https://bsky.social/oauth/token";

		// Create initial DPoP proof (without nonce)
		const { proof: dpopProof, publicKey: dpopPublicKey, keyPair: dpopKeyPair } =
			await oauthClient.createDPoPProof("POST", tokenUrl);

		// Build token request parameters
		const tokenParams = new URLSearchParams({
			grant_type: "authorization_code",
			code,
			redirect_uri: `${origin}/api/auth/oauth2/callback/atproto`,
			client_id: `${origin}/client-metadata.json`,
			client_assertion_type: "urn:ietf:params:oauth:client-assertion-type:jwt-bearer",
			client_assertion: clientAssertion,
			code_verifier: codeVerifier, // Include PKCE code verifier
		});

		// Get cached DPoP nonce if available
		let authServerNonce: string | undefined;
		const userCacheKV = locals.runtime.env.USER_CACHE;
		if (userCacheKV) {
			try {
				const cachedNonce = await userCacheKV.get('dpop_nonce:auth_server');
				if (cachedNonce) {
					authServerNonce = cachedNonce;
					console.log("Using cached auth server DPoP nonce:", authServerNonce);
				}
			} catch (e) {
				console.error("Failed to get cached nonce:", e);
			}
		}

		// Make token request with DPoP (including nonce if we have it)
		let tokenRes = await fetch(tokenUrl, {
			method: "POST",
			headers: {
				"Content-Type": "application/x-www-form-urlencoded",
				"DPoP": authServerNonce
					? (await oauthClient.createDPoPProof("POST", tokenUrl, authServerNonce, dpopKeyPair)).proof
					: dpopProof,
			},
			body: tokenParams.toString(),
		});

		console.log("Token response status:", tokenRes.status);
		console.log("Token response headers:", Object.fromEntries(tokenRes.headers.entries()));

		// If we get a 401 or 400, check for DPoP-Nonce and retry
		if (tokenRes.status === 401 || tokenRes.status === 400) {
			const dpopNonce = tokenRes.headers.get("DPoP-Nonce") || tokenRes.headers.get("dpop-nonce");
			const responseText = await tokenRes.text();
			console.log("Error response:", responseText);

			if (dpopNonce) {
				console.log("Retrying with DPoP nonce:", dpopNonce);
				// Cache the nonce for future requests
				if (userCacheKV) {
					try {
						await userCacheKV.put('dpop_nonce:auth_server', dpopNonce, { expirationTtl: 300 }); // 5 minutes
					} catch (e) {
						console.error("Failed to cache nonce:", e);
					}
				}
				const retryProof = await oauthClient.createDPoPProof("POST", tokenUrl, dpopNonce, dpopKeyPair);

				tokenRes = await fetch(tokenUrl, {
					method: "POST",
					headers: {
						"Content-Type": "application/x-www-form-urlencoded",
						"DPoP": retryProof.proof,
					},
					body: tokenParams.toString(),
				});
				console.log("Retry response status:", tokenRes.status);
			}
		}

		// Update cached nonce from successful response
		const newNonce = tokenRes.headers.get("DPoP-Nonce") || tokenRes.headers.get("dpop-nonce");
		if (newNonce && userCacheKV) {
			try {
				await userCacheKV.put('dpop_nonce:auth_server', newNonce, { expirationTtl: 300 });
			} catch (e) {
				console.error("Failed to cache nonce:", e);
			}
		}

		if (!tokenRes.ok) {
			const error = await tokenRes.text();
			console.error("Token exchange failed:", tokenRes.status, error);
			return redirect("/api/auth/error?error=oauth_code_verification_failed");
		}

		const tokens = await tokenRes.json();
		console.log("Token exchange successful:", tokens);

		// Extract user DID from the token response
		const userDid = tokens.sub;
		if (!userDid) {
			console.error("No user DID in token response");
			return redirect("/api/auth/error?error=invalid_token");
		}

		// Resolve DID document to get PDS server
		let pdsUrl = "";
		let userProfile: any = {};
		let userEmail: string | null = null;
		try {
			// For did:plc DIDs, use plc.directory
			if (userDid.startsWith('did:plc:')) {
				const didDocRes = await fetch(`https://plc.directory/${userDid}`);
				if (didDocRes.ok) {
					const didDoc = await didDocRes.json();
					console.log("DID document:", didDoc);

					// Find the PDS service endpoint
					const pdsService = didDoc.service?.find((s: any) =>
						s.id === '#atproto_pds' || s.type === 'AtprotoPersonalDataServer'
					);

					if (pdsService?.serviceEndpoint) {
						pdsUrl = pdsService.serviceEndpoint;
						console.log("Found PDS URL:", pdsUrl);
					}
				}
			}

			// If we have a PDS URL, try to fetch the user's session info (includes email with transition:email scope)
			if (pdsUrl) {
				// First try to get session info which includes email
				const sessionUrl = `${pdsUrl}/xrpc/com.atproto.server.getSession`;
				let dpopProofForSession = await oauthClient.createDPoPProof(
					'GET',
					sessionUrl,
					undefined,
					dpopKeyPair,
					tokens.access_token // Include access token for ath field
				);

				let sessionRes = await fetch(sessionUrl, {
					headers: {
						'Authorization': `DPoP ${tokens.access_token}`,
						'DPoP': dpopProofForSession.proof
					}
				});

				// Handle DPoP nonce requirement
				if (sessionRes.status === 401) {
					const dpopNonce = sessionRes.headers.get('DPoP-Nonce') || sessionRes.headers.get('dpop-nonce');
					if (dpopNonce) {
						console.log("Retrying getSession with DPoP nonce:", dpopNonce);
						dpopProofForSession = await oauthClient.createDPoPProof(
							'GET',
							sessionUrl,
							dpopNonce,
							dpopKeyPair,
							tokens.access_token // Include access token for ath field
						);

						sessionRes = await fetch(sessionUrl, {
							headers: {
								'Authorization': `DPoP ${tokens.access_token}`,
								'DPoP': dpopProofForSession.proof
							}
						});
					}
				}

				if (sessionRes.ok) {
					const sessionData = await sessionRes.json();
					console.log("User session data:", sessionData);

					// Extract email and other info from session
					userEmail = sessionData.email || null;
					userProfile.handle = sessionData.handle;
					userProfile.emailConfirmed = sessionData.emailConfirmed;
				} else {
					const errorText = await sessionRes.text();
					console.warn("Failed to fetch session (email may not be available):", sessionRes.status, errorText);
				}

				// Now fetch the user's profile record
				const profileUrl = `${pdsUrl}/xrpc/com.atproto.repo.getRecord?repo=${userDid}&collection=app.bsky.actor.profile&rkey=self`;
				const dpopProofForProfile = await oauthClient.createDPoPProof(
					'GET',
					profileUrl,
					undefined,
					dpopKeyPair,
					tokens.access_token // Include access token for ath field
				);

				const profileRes = await fetch(profileUrl, {
					headers: {
						'Authorization': `DPoP ${tokens.access_token}`,
						'DPoP': dpopProofForProfile.proof
					}
				});

				if (profileRes.ok) {
					const profileData = await profileRes.json();
					console.log("User profile record:", profileData);

					if (profileData.value) {
						// Extract avatar URL from blob object
						let avatarUrl = null;
						if (profileData.value.avatar?.ref) {
							// Construct CDN URL for the avatar
							const cid = profileData.value.avatar.ref.$link || profileData.value.avatar.ref;
							avatarUrl = `https://cdn.bsky.app/img/avatar/plain/${userDid}/${cid}@jpeg`;
						}

						userProfile = {
							...userProfile,
							handle: userProfile.handle || profileData.value.handle || profileData.uri?.split('/')[2]?.replace('app.bsky.actor.profile', ''),
							displayName: profileData.value.displayName,
							avatar: avatarUrl,
							description: profileData.value.description
						};
					}
				} else {
					const errorText = await profileRes.text();
					console.warn("Failed to fetch user profile:", profileRes.status, errorText);
				}
			}
		} catch (e) {
			console.error("Failed to fetch user profile:", e);
		}

		// Create a Better Auth instance
		const auth = await createAuth(locals.runtime.env.DB, locals.runtime.env, request);
		const context = await auth.$context;
		const internalAdapter = context.internalAdapter;

		// Prepare user info
		const handle = userProfile.handle || userDid;
		const email = userEmail || `${handle}@atproto.local`;

		// Find or create user
		let user = await internalAdapter.findUserByEmail(email).then(result => result?.user || result);

		if (!user) {
			// Create new user
			user = await internalAdapter.createUser({
				email: email.toLowerCase(),
				name: userProfile.displayName || handle,
				image: userProfile.avatar || null,
				emailVerified: !!userEmail && userProfile.emailConfirmed,
			});
			console.log("Created new user:", user);

			// Link account
			await internalAdapter.linkAccount({
				providerId: "atproto",
				accountId: userDid,
				userId: user.id,
				accessToken: tokens.access_token,
				refreshToken: tokens.refresh_token,
				accessTokenExpiresAt: tokens.expires_in ? new Date(Date.now() + tokens.expires_in * 1000) : undefined,
				scope: tokens.scope,
			}, context);
		} else {
			console.log("Found existing user:", user);
			// Update user info if needed
			if (userProfile.displayName || userProfile.avatar || userEmail) {
				const updateData: any = {};
				if (userProfile.displayName) updateData.name = userProfile.displayName;
				if (userProfile.avatar) updateData.image = userProfile.avatar;
				if (userEmail) {
					updateData.email = userEmail.toLowerCase();
					if (userProfile.emailConfirmed !== undefined) {
						updateData.emailVerified = userProfile.emailConfirmed;
					}
				}
				await internalAdapter.updateUser(user.id, updateData).catch(e => {
					console.error("Failed to update user:", e);
				});
			}

			// Update or create account link
			const account = await internalAdapter.findAccountByProviderId(userDid, "atproto").then(result => result?.account || result);
			if (account) {
				await internalAdapter.updateAccount(account.id, {
					accessToken: tokens.access_token,
					refreshToken: tokens.refresh_token,
					accessTokenExpiresAt: tokens.expires_in ? new Date(Date.now() + tokens.expires_in * 1000) : undefined,
				});
			} else {
				await internalAdapter.linkAccount({
					providerId: "atproto",
					accountId: userDid,
					userId: user.id,
					accessToken: tokens.access_token,
					refreshToken: tokens.refresh_token,
					accessTokenExpiresAt: tokens.expires_in ? new Date(Date.now() + tokens.expires_in * 1000) : undefined,
					scope: tokens.scope,
				}, context);
			}
		}

		// Create session
		const session = await internalAdapter.createSession(user.id, context);
		if (!session) {
			throw new Error("Failed to create session");
		}
		console.log("Created session:", session);

		// Sign the session token using the same serializer Better Auth expects
		const sessionToken = session.token;
		const resolvedSecret = context.secret;
		if (!resolvedSecret) {
			throw new Error("Missing auth secret in context");
		}
		const signedToken = (await serializeSignedCookie("", sessionToken, resolvedSecret)).replace("=", "");

		// Build Set-Cookie using Better Auth's configured cookie name and attributes
		const cookieName = context.authCookies.sessionToken.name; // may include __Secure- prefix
		const cookieOpts = context.authCookies.sessionToken.options;
		const parts: string[] = [];
		parts.push(`${cookieName}=${signedToken}`);
		if (cookieOpts.path) parts.push(`Path=${cookieOpts.path}`);
		if (cookieOpts.domain) parts.push(`Domain=${cookieOpts.domain}`);
		if (cookieOpts.httpOnly) parts.push('HttpOnly');
		if (cookieOpts.secure) parts.push('Secure');
		if (cookieOpts.sameSite) parts.push(`SameSite=${String(cookieOpts.sameSite).charAt(0).toUpperCase()}${String(cookieOpts.sameSite).slice(1)}`);
		if (cookieOpts.maxAge) parts.push(`Max-Age=${cookieOpts.maxAge}`);
		const sessionCookie = parts.join('; ');

		// Additional cookies for AT Protocol specific data
		const additionalCookies = [
			`atproto_dpop_key=${encodeURIComponent(JSON.stringify(dpopPublicKey))}; Path=/; HttpOnly; Secure; SameSite=Lax; Max-Age=604800`,
			`atproto_user_did=${encodeURIComponent(userDid)}; Path=/; HttpOnly; Secure; SameSite=Lax; Max-Age=604800`,
			`atproto_handle=${encodeURIComponent(handle)}; Path=/; HttpOnly; Secure; SameSite=Lax; Max-Age=604800`,
			`atproto_pds_url=${encodeURIComponent(pdsUrl)}; Path=/; HttpOnly; Secure; SameSite=Lax; Max-Age=604800`
		];

		// Cache user data in KV for future use
		if (userCacheKV) {
			try {
				// Store user data for 30 days
				await userCacheKV.put(
					`user:${userDid}`,
					JSON.stringify({
						handle: userProfile.handle || handle,
						displayName: userProfile.displayName,
						avatar: userProfile.avatar,
						email: userEmail,
						emailConfirmed: userProfile.emailConfirmed,
						pdsUrl,
						updatedAt: new Date().toISOString()
					}),
					{ expirationTtl: 60 * 60 * 24 * 30 } // 30 days
				);
			} catch (e) {
				console.error("Failed to cache user data:", e);
			}
		}

		// Clear the OAuth state cookies
		additionalCookies.push('atproto_oauth_state=; Path=/; Max-Age=0');
		additionalCookies.push('atproto_oauth_verifier=; Path=/; Max-Age=0');

		const headers = new Headers({
			"Location": "/dashboard"
		});

		// Set session cookie
		headers.append("Set-Cookie", sessionCookie);

		// Set additional AT Protocol specific cookies
		additionalCookies.forEach(cookie => {
			headers.append("Set-Cookie", cookie);
		});

		return new Response(null, {
			status: 302,
			headers
		});

	} catch (error) {
		console.error("Callback error:", error);
		return redirect("/api/auth/error?error=server_error");
	}
};
