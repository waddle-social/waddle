/**
 * Parse a JSON document while rejecting duplicate object keys.
 *
 * `JSON.parse` silently keeps the last duplicate property. That is unsafe for
 * evidence: an earlier shadowed value could contain raw identifiers even when
 * the parsed object looks valid. This lightweight structural scan decodes each
 * object key exactly as JSON does, then delegates full syntax/value validation
 * to the native parser.
 */
export function parseJsonDocument(value: string, label: string): unknown {
	assertNoDuplicateObjectKeys(value, label);
	try {
		return JSON.parse(value) as unknown;
	} catch {
		throw new Error(`${label} does not contain valid JSON`);
	}
}

function assertNoDuplicateObjectKeys(value: string, label: string): void {
	let offset = 0;

	const skipWhitespace = () => {
		while (offset < value.length && /\s/.test(value[offset])) offset += 1;
	};

	const parseString = (): string => {
		const start = offset;
		if (value[offset] !== '"') throw new Error(`${label} does not contain valid JSON`);
		offset += 1;
		while (offset < value.length) {
			const character = value[offset];
			if (character === "\\") {
				offset += 2;
				continue;
			}
			offset += 1;
			if (character === '"') {
				try {
					return JSON.parse(value.slice(start, offset)) as string;
				} catch {
					throw new Error(`${label} does not contain valid JSON`);
				}
			}
		}
		throw new Error(`${label} does not contain valid JSON`);
	};

	const parseValue = (): void => {
		skipWhitespace();
		if (value[offset] === "{") {
			parseObject();
			return;
		}
		if (value[offset] === "[") {
			parseArray();
			return;
		}
		if (value[offset] === '"') {
			parseString();
			return;
		}
		while (
			offset < value.length
			&& value[offset] !== ","
			&& value[offset] !== "]"
			&& value[offset] !== "}"
		) offset += 1;
	};

	const parseArray = (): void => {
		offset += 1;
		skipWhitespace();
		if (value[offset] === "]") {
			offset += 1;
			return;
		}
		while (offset < value.length) {
			parseValue();
			skipWhitespace();
			if (value[offset] === "]") {
				offset += 1;
				return;
			}
			if (value[offset] !== ",") throw new Error(`${label} does not contain valid JSON`);
			offset += 1;
		}
		throw new Error(`${label} does not contain valid JSON`);
	};

	const parseObject = (): void => {
		offset += 1;
		const keys = new Set<string>();
		skipWhitespace();
		if (value[offset] === "}") {
			offset += 1;
			return;
		}
		while (offset < value.length) {
			skipWhitespace();
			const key = parseString();
			if (keys.has(key)) throw new Error(`${label} contains a duplicate object key`);
			keys.add(key);
			skipWhitespace();
			if (value[offset] !== ":") throw new Error(`${label} does not contain valid JSON`);
			offset += 1;
			parseValue();
			skipWhitespace();
			if (value[offset] === "}") {
				offset += 1;
				return;
			}
			if (value[offset] !== ",") throw new Error(`${label} does not contain valid JSON`);
			offset += 1;
		}
		throw new Error(`${label} does not contain valid JSON`);
	};

	try {
		parseValue();
		skipWhitespace();
		if (offset !== value.length) throw new Error(`${label} does not contain valid JSON`);
	} catch (error) {
		if (error instanceof Error && error.message.startsWith(label)) throw error;
		throw new Error(`${label} does not contain valid JSON`);
	}
}
