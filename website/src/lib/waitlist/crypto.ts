const TOKEN_BYTES = 32;
const textEncoder = new TextEncoder();

function bytesToBase64(bytes: Uint8Array): string {
	let output = "";
	for (const byte of bytes) {
		output += String.fromCharCode(byte);
	}
	return btoa(output);
}

function bytesToHex(bytes: Uint8Array): string {
	return Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0")).join("");
}

export function normalizeEmail(input: string): string {
	return input.trim().toLowerCase();
}

export function isValidEmail(input: string): boolean {
	const email = normalizeEmail(input);
	return email.length > 3 && email.length <= 320 && /^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(email);
}

export function createOpaqueToken(): string {
	const bytes = new Uint8Array(TOKEN_BYTES);
	crypto.getRandomValues(bytes);
	return bytesToBase64(bytes)
		.replace(/\+/g, "-")
		.replace(/\//g, "_")
		.replace(/=+$/u, "");
}

export async function hashOpaqueToken(token: string): Promise<string> {
	const digest = await crypto.subtle.digest("SHA-256", textEncoder.encode(token));
	return bytesToHex(new Uint8Array(digest));
}
