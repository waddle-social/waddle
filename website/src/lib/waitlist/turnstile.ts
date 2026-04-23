const TURNSTILE_SITEVERIFY_URL =
	"https://challenges.cloudflare.com/turnstile/v0/siteverify";

type TurnstileSiteverifyResponse = {
	success: boolean;
	action?: string;
	hostname?: string;
	["error-codes"]?: string[];
};

export type TurnstileVerificationResult =
	| {
			kind: "verified";
			metadata: TurnstileSiteverifyResponse;
	  }
	| {
			kind: "failed";
			errorCodes: string[];
	  };

export async function verifyTurnstileToken(
	input: {
		token: string | null;
		secretKey: string | null | undefined;
		remoteIp?: string | null;
		expectedAction?: string;
	},
	fetchImpl: typeof fetch = fetch,
): Promise<TurnstileVerificationResult> {
	const token = input.token?.trim() ?? "";
	const secretKey = input.secretKey?.trim() ?? "";

	if (!secretKey) {
		return { kind: "failed", errorCodes: ["missing-input-secret"] };
	}

	if (!token) {
		return { kind: "failed", errorCodes: ["missing-input-response"] };
	}

	const formData = new FormData();
	formData.append("secret", secretKey);
	formData.append("response", token);
	formData.append("idempotency_key", crypto.randomUUID());

	if (input.remoteIp) {
		formData.append("remoteip", input.remoteIp);
	}

	try {
		const response = await fetchImpl(TURNSTILE_SITEVERIFY_URL, {
			method: "POST",
			body: formData,
		});

		if (!response.ok) {
			return { kind: "failed", errorCodes: ["internal-error"] };
		}

		const payload = (await response.json()) as TurnstileSiteverifyResponse;
		if (!payload.success) {
			return {
				kind: "failed",
				errorCodes: payload["error-codes"] ?? ["invalid-input-response"],
			};
		}

		if (input.expectedAction && payload.action !== input.expectedAction) {
			return { kind: "failed", errorCodes: ["action-mismatch"] };
		}

		return { kind: "verified", metadata: payload };
	} catch {
		return { kind: "failed", errorCodes: ["internal-error"] };
	}
}
