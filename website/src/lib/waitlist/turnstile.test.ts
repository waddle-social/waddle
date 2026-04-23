import { afterEach, describe, expect, it } from "bun:test";

import { WAITLIST_TURNSTILE_ACTION } from "./constants";
import { verifyTurnstileToken } from "./turnstile";

const originalFetch = globalThis.fetch;

afterEach(() => {
	globalThis.fetch = originalFetch;
});

describe("turnstile verification", () => {
	it("verifies a successful Siteverify response", async () => {
		globalThis.fetch = (async (input, init) => {
			expect(input).toBe("https://challenges.cloudflare.com/turnstile/v0/siteverify");
			expect(init?.method).toBe("POST");

			const body = init?.body;
			expect(body).toBeInstanceOf(FormData);

			const formData = body as FormData;
			expect(formData.get("secret")).toBe("super-secret");
			expect(formData.get("response")).toBe("token-123");
			expect(formData.get("remoteip")).toBe("203.0.113.10");
			expect(formData.get("idempotency_key")).toEqual(expect.any(String));

			return Response.json({
				success: true,
				action: WAITLIST_TURNSTILE_ACTION,
			});
		}) as typeof fetch;

		const result = await verifyTurnstileToken({
			token: "token-123",
			secretKey: "super-secret",
			remoteIp: "203.0.113.10",
			expectedAction: WAITLIST_TURNSTILE_ACTION,
		});

		expect(result.kind).toBe("verified");
	});

	it("fails closed when the token is missing", async () => {
		let called = false;

		globalThis.fetch = (async () => {
			called = true;
			return Response.json({ success: true });
		}) as typeof fetch;

		const result = await verifyTurnstileToken({
			token: null,
			secretKey: "super-secret",
			expectedAction: WAITLIST_TURNSTILE_ACTION,
		});

		expect(result).toEqual({
			kind: "failed",
			errorCodes: ["missing-input-response"],
		});
		expect(called).toBeFalse();
	});

	it("fails when Cloudflare accepts the token for a different action", async () => {
		globalThis.fetch = (async () =>
			Response.json({
				success: true,
				action: "something_else",
			})) as typeof fetch;

		const result = await verifyTurnstileToken({
			token: "token-123",
			secretKey: "super-secret",
			expectedAction: WAITLIST_TURNSTILE_ACTION,
		});

		expect(result).toEqual({
			kind: "failed",
			errorCodes: ["action-mismatch"],
		});
	});

	it("fails closed when Siteverify errors", async () => {
		globalThis.fetch = (async () => {
			throw new Error("network down");
		}) as typeof fetch;

		const result = await verifyTurnstileToken({
			token: "token-123",
			secretKey: "super-secret",
			expectedAction: WAITLIST_TURNSTILE_ACTION,
		});

		expect(result).toEqual({
			kind: "failed",
			errorCodes: ["internal-error"],
		});
	});
});
