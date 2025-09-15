import type { APIRoute } from "astro";

export const GET: APIRoute = async ({ url }) => {
	const origin = url.origin;

	const metadata = {
		client_id: `${origin}/client-metadata.json`,
		application_type: "web",
		client_name: "Waddle Colony",
		redirect_uris: [`${origin}/api/auth/oauth2/callback/atproto`],
		grant_types: ["authorization_code", "refresh_token"],
		response_types: ["code"],
		scope: "atproto transition:email",
		dpop_bound_access_tokens: true,
		token_endpoint_auth_method: "private_key_jwt",
		jwks_uri: `${origin}/jwks.json`,
		token_endpoint_auth_signing_alg: "ES256",
	};

	return new Response(JSON.stringify(metadata), {
		status: 200,
		headers: {
			"Content-Type": "application/json",
			"Cache-Control": "public, max-age=3600", // Cache for 1 hour
		},
	});
};
