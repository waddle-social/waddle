import type { APIRoute } from "astro";
import { buildRedirectCookie, clearRedirectCookie, resolveRedirectTarget } from "../../../../lib/auth/redirect";

// Generate code verifier for PKCE (43-128 characters)
function generateCodeVerifier(): string {
	const array = new Uint8Array(32);
	crypto.getRandomValues(array);
	// Convert to base64url without padding
	const base64 = btoa(String.fromCharCode(...array))
		.replace(/\+/g, '-')
		.replace(/\//g, '_')
		.replace(/=/g, '');

	// Ensure minimum length of 43 characters
	if (base64.length < 43) {
		// This should not happen with 32 bytes, but just in case
		const additional = new Uint8Array(16);
		crypto.getRandomValues(additional);
		return base64 + btoa(String.fromCharCode(...additional))
			.replace(/\+/g, '-')
			.replace(/\//g, '_')
			.replace(/=/g, '');
	}
	return base64;
}

// Generate code challenge from verifier
async function generateCodeChallenge(verifier: string): Promise<string> {
	const encoder = new TextEncoder();
	const data = encoder.encode(verifier);
	const digest = await crypto.subtle.digest('SHA-256', data);
	return btoa(String.fromCharCode(...new Uint8Array(digest)))
		.replace(/\+/g, '-')
		.replace(/\//g, '_')
		.replace(/=/g, '');
}

export const POST: APIRoute = async ({ request, url, locals }) => {
	try {
		const body = await request.json();
		const handle = typeof body.handle === 'string' ? body.handle : '';
		const redirectCandidate = typeof body.redirectTo === 'string' ? body.redirectTo : null;

		if (!handle) {
			return new Response(
				JSON.stringify({ error: "Handle is required" }),
				{
					status: 400,
					headers: { "Content-Type": "application/json" },
				}
			);
		}

		// Since Better Auth doesn't support DPoP properly, we'll handle the OAuth flow ourselves
		const origin = url.origin;

		// Generate state and PKCE parameters
		const state = crypto.randomUUID();
		const codeVerifier = generateCodeVerifier();
		const codeChallenge = await generateCodeChallenge(codeVerifier);

		// Build the authorization URL
		const authParams = new URLSearchParams({
			response_type: "code",
			client_id: `${origin}/client-metadata.json`,
			redirect_uri: `${origin}/api/auth/oauth2/callback/atproto`,
			state,
			scope: "atproto transition:email",
			code_challenge: codeChallenge,
			code_challenge_method: "S256",
			login_hint: handle,
		});

		const authUrl = `https://bsky.social/oauth/authorize?${authParams.toString()}`;

		// Log for debugging
		console.log("Generated code verifier length:", codeVerifier.length);
		console.log("Generated code verifier:", codeVerifier);

		// Store state and code verifier in cookies for the callback
		// Use URL encoding to ensure special characters are preserved
		const cookies = [
			`atproto_oauth_state=${encodeURIComponent(state)}; Path=/; HttpOnly; Secure; SameSite=Lax; Max-Age=600`,
			`atproto_oauth_verifier=${encodeURIComponent(codeVerifier)}; Path=/; HttpOnly; Secure; SameSite=Lax; Max-Age=600`
		];

		const redirectTarget = resolveRedirectTarget(redirectCandidate, origin, locals.runtime.env ?? {});
		if (redirectTarget) {
			cookies.push(buildRedirectCookie(redirectTarget, origin));
		} else {
			cookies.push(clearRedirectCookie(origin));
		}

		// Create headers with multiple Set-Cookie headers
		const headers = new Headers({
			"Content-Type": "application/json"
		});

		// Add each cookie as a separate Set-Cookie header
		cookies.forEach(cookie => {
			headers.append("Set-Cookie", cookie);
		});

		return new Response(
			JSON.stringify({ authUrl }),
			{
				status: 200,
				headers
			}
		);
	} catch (error) {
		console.error("Error initiating OAuth flow:", error);
		return new Response(
			JSON.stringify({ error: "Failed to initiate authentication" }),
			{
				status: 500,
				headers: { "Content-Type": "application/json" },
			}
		);
	}
};
