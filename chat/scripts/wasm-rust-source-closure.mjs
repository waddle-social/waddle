import { canonicalRelativePath } from "./wasm-build-input-digest.mjs";

const COMPILE_TIME_INCLUDE = /\binclude(?:_bytes|_str)?\b/u;
const CONDITIONAL_PATH_ATTRIBUTE = /#\s*\[\s*cfg_attr\b[^\]]*\bpath\s*=/u;
const DIRECT_PATH_ATTRIBUTE = /#\s*\[\s*path\s*=/gu;
const LITERAL_PATH_ATTRIBUTE = /^#\s*\[\s*path\s*=\s*"([^"]+)"\s*\]$/u;

function maskRange(masked, source, start, end) {
	for (let index = start; index < end; index += 1) {
		if (source[index] !== "\n" && source[index] !== "\r") {
			masked[index] = " ";
		}
	}
}

function quotedLiteralEnd(source, quoteIndex, quote) {
	let escaped = false;
	for (let index = quoteIndex + 1; index < source.length; index += 1) {
		const character = source[index];
		if (quote === "'" && (character === "\n" || character === "\r")) {
			return undefined;
		}
		if (escaped) {
			escaped = false;
			continue;
		}
		if (character === "\\") {
			escaped = true;
			continue;
		}
		if (character === quote) return index + 1;
	}
	return undefined;
}

function characterLiteralEnd(source, quoteIndex) {
	let index = quoteIndex + 1;
	if (source[index] === "\\") {
		index += 1;
		if (source[index] === "u" && source[index + 1] === "{") {
			const closeBrace = source.indexOf("}", index + 2);
			if (closeBrace < 0) return undefined;
			index = closeBrace + 1;
		} else if (source[index] === "x") {
			index += 3;
		} else {
			index += 1;
		}
	} else {
		const codePoint = source.codePointAt(index);
		if (
			codePoint === undefined ||
			source[index] === "\n" ||
			source[index] === "\r"
		) {
			return undefined;
		}
		index += codePoint > 0xffff ? 2 : 1;
	}
	return source[index] === "'" ? index + 1 : undefined;
}

function rawStringEnd(source, index) {
	const match = /^(?:br|cr|r)(#{0,255})"/u.exec(source.slice(index));
	if (!match) return undefined;
	const terminator = `"${match[1]}`;
	const terminatorIndex = source.indexOf(terminator, index + match[0].length);
	if (terminatorIndex < 0) {
		throw new Error("unterminated raw string literal in WASM Rust source");
	}
	return terminatorIndex + terminator.length;
}

function rustCodeWithoutCommentsAndLiterals(source) {
	const masked = source.split("");
	let index = 0;
	while (index < source.length) {
		const rawEnd = rawStringEnd(source, index);
		if (rawEnd !== undefined) {
			maskRange(masked, source, index, rawEnd);
			index = rawEnd;
			continue;
		}

		const character = source[index];
		const next = source[index + 1];
		if (character === "/" && next === "/") {
			const newline = source.indexOf("\n", index + 2);
			const end = newline < 0 ? source.length : newline;
			maskRange(masked, source, index, end);
			index = end;
			continue;
		}
		if (character === "/" && next === "*") {
			const start = index;
			let depth = 1;
			index += 2;
			while (index < source.length && depth > 0) {
				if (source[index] === "/" && source[index + 1] === "*") {
					depth += 1;
					index += 2;
				} else if (source[index] === "*" && source[index + 1] === "/") {
					depth -= 1;
					index += 2;
				} else {
					index += 1;
				}
			}
			if (depth !== 0) {
				throw new Error("unterminated block comment in WASM Rust source");
			}
			maskRange(masked, source, start, index);
			continue;
		}

		const prefixedDoubleQuote =
			(character === "b" || character === "c") && next === '"';
		if (character === '"' || prefixedDoubleQuote) {
			const quoteIndex = prefixedDoubleQuote ? index + 1 : index;
			const end = quotedLiteralEnd(source, quoteIndex, '"');
			if (end === undefined) {
				throw new Error("unterminated string literal in WASM Rust source");
			}
			maskRange(masked, source, index, end);
			index = end;
			continue;
		}

		const prefixedSingleQuote = character === "b" && next === "'";
		if (character === "'" || prefixedSingleQuote) {
			const quoteIndex = prefixedSingleQuote ? index + 1 : index;
			const end = characterLiteralEnd(source, quoteIndex);
			if (end !== undefined) {
				maskRange(masked, source, index, end);
				index = end;
				continue;
			}
		}

		index += 1;
	}
	return masked.join("");
}

function directPathOverrides(source, code, path) {
	const overrides = [];
	for (const match of code.matchAll(DIRECT_PATH_ATTRIBUTE)) {
		const end = code.indexOf("]", match.index + match[0].length);
		if (end < 0) {
			throw new Error(`unterminated Rust #[path] override in ${path}`);
		}
		const attribute = source.slice(match.index, end + 1);
		const literal = LITERAL_PATH_ATTRIBUTE.exec(attribute);
		if (!literal) {
			throw new Error(`unsupported dynamic Rust #[path] override in ${path}`);
		}
		overrides.push(literal[1]);
	}
	return overrides;
}

export function validateWasmRustSourceClosure(entries) {
	const decoder = new TextDecoder("utf-8", { fatal: true });
	const includedPaths = new Set(entries.keys());
	for (const [path, bytes] of entries) {
		if (!path.endsWith(".rs")) continue;
		let source;
		try {
			source = decoder.decode(bytes);
		} catch {
			throw new Error(`WASM Rust source must be valid UTF-8: ${path}`);
		}
		const code = rustCodeWithoutCommentsAndLiterals(source);
		if (COMPILE_TIME_INCLUDE.test(code)) {
			throw new Error(
				`unsupported compile-time include macro in ${path}; declare the included input and extend the manifest validator`,
			);
		}
		if (CONDITIONAL_PATH_ATTRIBUTE.test(code)) {
			throw new Error(
				`unsupported conditional Rust #[cfg_attr(..., path = ...)] override in ${path}`,
			);
		}

		const pathOverrides = directPathOverrides(source, code, path);
		const sourceDirectory = path.split("/").slice(0, -1).join("/");
		for (const override of pathOverrides) {
			const referencedPath = canonicalRelativePath(override);
			const resolvedPath = canonicalRelativePath(
				`${sourceDirectory}/${referencedPath}`,
			);
			if (!includedPaths.has(resolvedPath)) {
				throw new Error(
					`Rust #[path] override in ${path} is outside the declared WASM source closure: ${resolvedPath}`,
				);
			}
		}
	}
}
