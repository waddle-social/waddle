import { createHash } from "node:crypto";

const LENGTH_BYTES = 8;

export function canonicalRelativePath(path) {
	if (typeof path !== "string" || path.length === 0) {
		throw new TypeError("WASM build input paths must be non-empty strings");
	}
	if (path.includes("\\")) {
		throw new Error(`WASM build input path must use '/': ${path}`);
	}
	if (path.startsWith("/") || /^[A-Za-z]:\//u.test(path)) {
		throw new Error(`WASM build input path must be relative: ${path}`);
	}

	const segments = path.split("/");
	if (
		segments.some(
			(segment) => segment === "" || segment === "." || segment === "..",
		)
	) {
		throw new Error(`WASM build input path is not canonical: ${path}`);
	}
	return segments.join("/");
}

export function bytewiseCompare(left, right) {
	const leftBytes = Buffer.from(left, "utf8");
	const rightBytes = Buffer.from(right, "utf8");
	return leftBytes.compare(rightBytes);
}

function updateLength(hash, length) {
	const frame = Buffer.alloc(LENGTH_BYTES);
	frame.writeBigUInt64BE(BigInt(length));
	hash.update(frame);
}

function updateFrame(hash, bytes) {
	updateLength(hash, bytes.byteLength);
	hash.update(bytes);
}

/**
 * Hash canonical relative paths and file bytes with unambiguous framing.
 *
 * Bump the descriptor's digestFormat tag before changing path rules, framing,
 * hashing, manifest interpretation, or the declared build-tool contract. A
 * format bump requires regenerating the committed wrapper in the same change.
 */
export function digestCanonicalInputs(entries, digestFormat) {
	if (typeof digestFormat !== "string" || digestFormat.length === 0) {
		throw new TypeError("WASM build digest format must be a non-empty string");
	}

	const seen = new Set();
	const canonicalEntries = entries.map(({ path, bytes }) => {
		const canonicalPath = canonicalRelativePath(path);
		if (seen.has(canonicalPath)) {
			throw new Error(`duplicate WASM build input path: ${canonicalPath}`);
		}
		seen.add(canonicalPath);
		if (!(bytes instanceof Uint8Array)) {
			throw new TypeError(
				`WASM build input bytes must be Uint8Array: ${canonicalPath}`,
			);
		}
		return { path: canonicalPath, bytes: Buffer.from(bytes) };
	});
	canonicalEntries.sort((left, right) =>
		bytewiseCompare(left.path, right.path),
	);

	const hash = createHash("sha256");
	updateFrame(hash, Buffer.from(digestFormat, "utf8"));
	updateLength(hash, canonicalEntries.length);
	for (const entry of canonicalEntries) {
		updateFrame(hash, Buffer.from(entry.path, "utf8"));
		updateFrame(hash, entry.bytes);
	}
	return hash.digest("hex");
}
