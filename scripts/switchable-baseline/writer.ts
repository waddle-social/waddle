import { randomUUID } from "node:crypto";
import {
	link,
	lstat,
	mkdir,
	open,
	unlink,
} from "node:fs/promises";
import { resolve } from "node:path";
import {
	JSON_EVIDENCE_FILENAME,
	MARKDOWN_EVIDENCE_FILENAME,
} from "./model";

export interface EvidencePaths {
	jsonPath: string;
	markdownPath: string;
}

async function requireAbsent(path: string): Promise<void> {
	try {
		await lstat(path);
		throw new Error(`baseline collector refuses to replace existing output ${path}`);
	} catch (error) {
		if ((error as NodeJS.ErrnoException).code !== "ENOENT") throw error;
	}
}

async function syncDirectory(path: string): Promise<void> {
	const handle = await open(path, "r");
	try {
		await handle.sync();
	} finally {
		await handle.close();
	}
}

/**
 * Publish the staged Prometheus JSON and review without replacing a prior run.
 * Both files are fully fsynced before their canonical staging names appear;
 * any handled second-link failure removes the first link.
 */
export async function writeEvidencePairAtomically(
	outputDirectory: string,
	jsonEvidence: string,
	markdownEvidence: string,
): Promise<EvidencePaths> {
	await mkdir(outputDirectory, { recursive: true });
	const jsonPath = resolve(outputDirectory, JSON_EVIDENCE_FILENAME);
	const markdownPath = resolve(outputDirectory, MARKDOWN_EVIDENCE_FILENAME);
	await requireAbsent(jsonPath);
	await requireAbsent(markdownPath);

	const token = `${process.pid}-${randomUUID()}`;
	const stagedJsonPath = resolve(outputDirectory, `.${JSON_EVIDENCE_FILENAME}.${token}.tmp`);
	const stagedMarkdownPath = resolve(
		outputDirectory,
		`.${MARKDOWN_EVIDENCE_FILENAME}.${token}.tmp`,
	);
	const staged = [
		{ path: stagedJsonPath, contents: jsonEvidence },
		{ path: stagedMarkdownPath, contents: markdownEvidence },
	];
	const published: string[] = [];
	try {
		for (const entry of staged) {
			const handle = await open(entry.path, "wx", 0o644);
			try {
				await handle.writeFile(entry.contents, "utf8");
				await handle.sync();
			} finally {
				await handle.close();
			}
		}
		for (const [source, destination] of [
			[stagedJsonPath, jsonPath],
			[stagedMarkdownPath, markdownPath],
		] as const) {
			await link(source, destination);
			published.push(destination);
		}
		await syncDirectory(outputDirectory);
		return { jsonPath, markdownPath };
	} catch (error) {
		await Promise.all(published.map((path) => unlink(path).catch(() => undefined)));
		await syncDirectory(outputDirectory).catch(() => undefined);
		throw error;
	} finally {
		await Promise.all(staged.map(({ path }) => unlink(path).catch(() => undefined)));
	}
}
