import assert from "node:assert/strict";
import {
	createHash,
	createHmac,
	randomBytes,
	randomUUID,
	webcrypto,
} from "node:crypto";
import {
	mkdtempSync,
	openSync,
	closeSync,
	readFileSync,
	writeFileSync,
	rmSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const state = mkdtempSync(resolve(tmpdir(), "colony-oidc-smoke-"));
const persist = resolve(state, "state");
const wrangler = Bun.which("wrangler") ?? resolve(root, "node_modules/.bin/wrangler");
const configPath = resolve(state, "wrangler.json");
const authSecret = randomBytes(32).toString("hex");
const sessionToken = randomBytes(32).toString("hex");
const userId = "local-smoke-user";
const reserve = Bun.serve({
	hostname: "127.0.0.1",
	port: 0,
	fetch: () => new Response(),
});
const port = reserve.port;
reserve.stop(true);
const issuer = `http://127.0.0.1:${port}`;
const redirectUri = `${issuer}/smoke-callback`;
const environment = { ...process.env, WRANGLER_SEND_METRICS: "false" };
let worker;
let logFd;

async function run(args) {
	const child = Bun.spawn([wrangler, ...args, "--config", configPath], {
		cwd: state,
		env: environment,
		stdout: "pipe",
		stderr: "pipe",
	});
	const timeout = setTimeout(() => child.kill("SIGTERM"), 60_000);
	const [code, stdout, stderr] = await Promise.all([
		child.exited,
		new Response(child.stdout).text(),
		new Response(child.stderr).text(),
	]);
	clearTimeout(timeout);
	assert.equal(
		code,
		0,
		`Local Wrangler command failed: ${args[0]} ${args[1]}\n${stdout}\n${stderr}`,
	);
}

async function request(path, options = {}) {
	const url = new URL(path, issuer);
	assert.equal(
		url.origin,
		issuer,
		"Smoke requests must stay on the local worker",
	);
	return fetch(url, {
		...options,
		redirect: "manual",
		signal: AbortSignal.timeout(15_000),
	});
}

async function json(response, status = 200) {
	const body = await response.json();
	assert.equal(
		response.status,
		status,
		`Unexpected HTTP status (${body.error ?? "no OAuth error"})`,
	);
	return body;
}

const encode = (value) =>
	Buffer.from(JSON.stringify(value)).toString("base64url");
const keyPair = await webcrypto.subtle.generateKey(
	{ name: "ECDSA", namedCurve: "P-256" },
	true,
	["sign", "verify"],
);
const publicKey = await webcrypto.subtle.exportKey("jwk", keyPair.publicKey);
async function dpop(url) {
	const header = encode({ typ: "dpop+jwt", alg: "ES256", jwk: publicKey });
	const payload = encode({
		htm: "POST",
		htu: url,
		iat: Math.floor(Date.now() / 1000),
		jti: randomUUID(),
	});
	const input = `${header}.${payload}`;
	const signature = await webcrypto.subtle.sign(
		{ name: "ECDSA", hash: "SHA-256" },
		keyPair.privateKey,
		Buffer.from(input),
	);
	return `${input}.${Buffer.from(signature).toString("base64url")}`;
}

try {
	// Use the actual production bundle with exclusively throwaway local bindings.
	const production = JSON.parse(
		readFileSync(resolve(root, "dist/server/wrangler.json"), "utf8"),
	);
	writeFileSync(
		configPath,
		JSON.stringify({
			name: "colony-oidc-smoke",
			main: resolve(root, "dist/server", production.main),
			no_bundle: true,
			rules: production.rules,
			compatibility_date: production.compatibility_date,
			compatibility_flags: production.compatibility_flags,
			assets: { directory: resolve(root, "dist/client"), binding: "ASSETS" },
			vars: {
				BETTER_AUTH_URL: issuer,
				GITHUB_CLIENT_ID: "local-smoke-github-client",
			},
			d1_databases: [
				{
					binding: "DB",
					database_name: "colony-smoke",
					database_id: "00000000-0000-0000-0000-000000000001",
					migrations_dir: resolve(root, "drizzle"),
				},
			],
			kv_namespaces: [{ binding: "SESSION", id: "local-smoke-session" }],
			images: { binding: "IMAGES" },
			secrets_store_secrets: ["BETTER_AUTH_SECRET", "GITHUB_CLIENT_SECRET"].map(
				(name) => ({
					binding: name,
					store_id: "local-smoke-secrets",
					secret_name: name,
				}),
			),
		}),
	);
	await run([
		"d1",
		"migrations",
		"apply",
		"colony-smoke",
		"--local",
		"--persist-to",
		persist,
	]);
	// Each CLI invocation opens the same persisted store; serialize fixture writes.
	for (const [name, value] of [
		["BETTER_AUTH_SECRET", authSecret],
		["GITHUB_CLIENT_SECRET", "local-smoke-github-secret"],
	]) {
		await run([
			"secrets-store",
			"secret",
			"create",
			"local-smoke-secrets",
			"--name",
			name,
			"--value",
			value,
			"--scopes",
			"workers",
			"--persist-to",
			persist,
		]);
	}
	const now = Date.now();
	const seedPath = resolve(state, "session.sql");
	writeFileSync(
		seedPath,
		`INSERT INTO user (id,name,email,email_verified,created_at,updated_at,github_username) VALUES ('${userId}','Smoke User','smoke@example.test',1,${now},${now},'smoke-user');\nINSERT INTO session (id,expires_at,token,created_at,updated_at,user_id) VALUES ('local-smoke-session',${now + 3_600_000},'${sessionToken}',${now},${now},'${userId}');`,
	);
	await run([
		"d1",
		"execute",
		"colony-smoke",
		"--local",
		"--persist-to",
		persist,
		"--file",
		seedPath,
	]);
	logFd = openSync(resolve(state, "worker.log"), "w");
	worker = Bun.spawn(
		[
			wrangler,
			"dev",
			"--local",
			"--config",
			configPath,
			"--persist-to",
			persist,
			"--ip",
			"127.0.0.1",
			"--port",
			String(port),
			"--show-interactive-dev-session=false",
		],
		{ cwd: state, env: environment, stdout: logFd, stderr: logFd },
	);
	const deadline = Date.now() + 30_000;
	let discovery;
	// Retry after each completed probe; startup readiness checks are sequential.
	while (Date.now() < deadline) {
		try {
			const response = await request("/.well-known/openid-configuration");
			if (response.ok) {
				discovery = await response.json();
				break;
			}
		} catch {
			/* The local workerd listener is still starting. */
		}
		await Bun.sleep(100);
	}
	assert.ok(discovery, "Local built worker did not become ready");
	for (const name of [
		"authorization_endpoint",
		"token_endpoint",
		"userinfo_endpoint",
		"jwks_uri",
		"registration_endpoint",
	]) {
		assert.equal(
			new URL(discovery[name]).origin,
			issuer,
			`${name} must use the local issuer`,
		);
	}
	assert.ok(discovery.claims_supported.includes("preferred_username"));
	await json(await request("/.well-known/oauth-authorization-server"));
	const client = await json(
		await request(discovery.registration_endpoint, {
			method: "POST",
			headers: { "Content-Type": "application/json" },
			body: JSON.stringify({
				client_name: "Local OIDC smoke",
				redirect_uris: [redirectUri],
				token_endpoint_auth_method: "none",
				grant_types: ["authorization_code"],
				response_types: ["code"],
				scope: "openid profile email",
			}),
		}),
	);
	assert.ok(client.client_id);
	const verifier = randomBytes(32).toString("base64url");
	const authorization = new URL(discovery.authorization_endpoint);
	const nonce = randomUUID();
	const stateValue = randomUUID();
	authorization.search = new URLSearchParams({
		client_id: client.client_id,
		redirect_uri: redirectUri,
		response_type: "code",
		scope: "openid profile email",
		state: stateValue,
		nonce,
		code_challenge: createHash("sha256").update(verifier).digest("base64url"),
		code_challenge_method: "S256",
	});
	const anonymous = await request(authorization);
	assert.equal(anonymous.status, 302);
	assert.equal(
		new URL(anonymous.headers.get("location"), issuer).pathname,
		"/sign-in",
	);
	const signature = createHmac("sha256", authSecret)
		.update(sessionToken)
		.digest("base64");
	const signedSessionToken = `${sessionToken}.${signature}`;
	const cookie = `better-auth.session_token=${encodeURIComponent(signedSessionToken)}`;
	const session = await json(
		await request("/api/auth/get-session", { headers: { Cookie: cookie } }),
	);
	assert.equal(session.user.id, userId);
	async function authorizationCode(url, requireConsent = false) {
		const response = await request(url, { headers: { Cookie: cookie } });
		assert.equal(response.status, 302);
		let callback = new URL(response.headers.get("location"), issuer);
		if (requireConsent) assert.equal(callback.pathname, "/oauth/consent");
		if (callback.pathname === "/oauth/consent") {
			const consentPage = await request(callback, {
				headers: { Cookie: cookie },
			});
			assert.equal(consentPage.status, 200);
			assert.ok((await consentPage.text()).includes("Authorize access"));
			const approved = await json(
				await request("/api/auth/oauth2/consent", {
					method: "POST",
					headers: {
						Cookie: cookie,
						Origin: issuer,
						"Content-Type": "application/json",
					},
					body: JSON.stringify({
						accept: true,
						oauth_query: callback.searchParams.toString(),
					}),
				}),
			);
			callback = new URL(
				approved.redirect_uri ?? approved.redirectURI ?? approved.url,
			);
		}
		assert.equal(callback.origin + callback.pathname, redirectUri);
		assert.equal(
			callback.searchParams.get("state"),
			url.searchParams.get("state"),
		);
		const code = callback.searchParams.get("code");
		assert.ok(code, "Authorization must issue a code");
		return code;
	}
	const code = await authorizationCode(authorization, true);
	const tokenBody = new URLSearchParams({
		grant_type: "authorization_code",
		code,
		client_id: client.client_id,
		redirect_uri: redirectUri,
		code_verifier: verifier,
	});
	const missingDpop = await request(discovery.token_endpoint, {
		method: "POST",
		body: tokenBody,
	});
	assert.equal(missingDpop.status, 400);
	assert.equal((await missingDpop.json()).error, "invalid_dpop_proof");
	const validProof = await dpop(discovery.token_endpoint);
	const [proofHeader, proofPayload, proofSignature] = validProof.split(".");
	const invalidSignature = Buffer.from(proofSignature, "base64url");
	assert.equal(invalidSignature.length, 64);
	invalidSignature[0] ^= 1;
	const tamperedProof = [
		proofHeader,
		proofPayload,
		invalidSignature.toString("base64url"),
	].join(".");
	const tamperedDpop = await request(discovery.token_endpoint, {
		method: "POST",
		headers: { DPoP: tamperedProof },
		body: tokenBody,
	});
	assert.equal(tamperedDpop.status, 400);
	assert.equal((await tamperedDpop.json()).error, "invalid_dpop_proof");
	const malformedDpop = await request(discovery.token_endpoint, {
		method: "POST",
		headers: { DPoP: `${validProof}x` },
		body: tokenBody,
	});
	assert.equal(malformedDpop.status, 400);
	assert.equal((await malformedDpop.json()).error, "invalid_dpop_proof");
	const token = await json(
		await request(discovery.token_endpoint, {
			method: "POST",
			headers: {
				Origin: "https://oidc-smoke-client.example.test",
				DPoP: await dpop(discovery.token_endpoint),
			},
			body: tokenBody,
		}),
	);
	assert.ok(token.access_token && token.id_token);
	const jwks = await json(await request(discovery.jwks_uri));
	const [jwtHeader, jwtPayload, jwtSignature] = token.id_token.split(".");
	const header = JSON.parse(Buffer.from(jwtHeader, "base64url"));
	const claims = JSON.parse(Buffer.from(jwtPayload, "base64url"));
	const jwk = jwks.keys.find((key) => key.kid === header.kid);
	assert.ok(jwk, "ID token must be verifiable with advertised JWKS");
	assert.equal(header.alg, "EdDSA");
	const verifyingKey = await webcrypto.subtle.importKey(
		"jwk",
		jwk,
		{ name: "Ed25519" },
		false,
		["verify"],
	);
	assert.ok(
		await webcrypto.subtle.verify(
			"Ed25519",
			verifyingKey,
			Buffer.from(jwtSignature, "base64url"),
			Buffer.from(`${jwtHeader}.${jwtPayload}`),
		),
		"ID token signature must verify",
	);
	assert.equal(claims.iss, discovery.issuer);
	assert.equal(claims.aud, client.client_id);
	assert.equal(claims.sub, userId);
	assert.equal(claims.nonce, nonce);
	assert.equal(claims.preferred_username, "smoke-user");
	assert.ok(claims.exp > Date.now() / 1000);
	const userinfo = await json(
		await request(discovery.userinfo_endpoint, {
			headers: { Authorization: `Bearer ${token.access_token}` },
		}),
	);
	assert.equal(userinfo.sub, userId);
	assert.equal(userinfo.preferred_username, "smoke-user");
	assert.equal(userinfo.email, "smoke@example.test");
	const replay = await request(discovery.token_endpoint, {
		method: "POST",
		headers: {
			Origin: "https://oidc-smoke-client.example.test",
			DPoP: await dpop(discovery.token_endpoint),
		},
		body: tokenBody,
	});
	assert.equal(replay.status, 401, "Authorization codes must be single use");
	assert.equal((await replay.json()).error, "invalid_verification");
	const wrongVerifierBody = new URLSearchParams(tokenBody);
	wrongVerifierBody.set("code", await authorizationCode(authorization));
	wrongVerifierBody.set("code_verifier", randomBytes(32).toString("base64url"));
	const wrongVerifier = await request(discovery.token_endpoint, {
		method: "POST",
		headers: {
			Origin: "https://oidc-smoke-client.example.test",
			DPoP: await dpop(discovery.token_endpoint),
		},
		body: wrongVerifierBody,
	});
	assert.equal(
		wrongVerifier.status,
		401,
		"The code must be bound to its PKCE verifier",
	);
	assert.equal((await wrongVerifier.json()).error, "invalid_request");
	const social = await json(
		await request("/api/auth/sign-in/social", {
			method: "POST",
			headers: { Origin: issuer, "Content-Type": "application/json" },
			body: JSON.stringify({ provider: "github", callbackURL: issuer }),
		}),
	);
	const socialUrl = new URL(social.url);
	assert.equal(socialUrl.origin, "https://github.com");
	assert.equal(
		socialUrl.searchParams.get("client_id"),
		"local-smoke-github-client",
	);
	assert.equal(
		new URL(socialUrl.searchParams.get("redirect_uri")).origin,
		issuer,
	);
	console.log(
		"PASS: built Colony worker, local D1/Secrets Store, discovery, dynamic registration, session, consent, PKCE+DPoP code exchange, verified ID token/JWKS, userinfo, missing/tampered-DPoP/wrong-PKCE/code-replay rejection and GitHub sign-in URL generation (never followed)",
	);
} catch (error) {
	if (logFd !== undefined) {
		console.error("Local OIDC worker log (synthetic fixtures only):");
		console.error(readFileSync(resolve(state, "worker.log"), "utf8"));
	}
	throw error;
} finally {
	if (worker) {
		worker.kill("SIGTERM");
		await worker.exited;
	}
	if (logFd !== undefined) closeSync(logFd);
	rmSync(state, { recursive: true, force: true });
}
