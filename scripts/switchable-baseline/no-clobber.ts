import { randomUUID } from "node:crypto";
import { link, lstat, open, rmdir, unlink } from "node:fs/promises";
import { basename, dirname, resolve } from "node:path";
import { ensureSafeOutputParent } from "./filesystem";

export interface NoClobberFile {
	path: string;
	contents: string | Uint8Array;
}

interface PublishedFile {
	path: string;
	device: bigint | number;
	inode: bigint | number;
}

async function publishedFile(path: string): Promise<PublishedFile> {
	const stat = await lstat(path);
	return { path, device: stat.dev, inode: stat.ino };
}

async function stillOwnsPublishedFile(file: PublishedFile): Promise<boolean> {
	try {
		const stat = await lstat(file.path);
		return stat.dev === file.device && stat.ino === file.inode;
	} catch {
		return false;
	}
}

async function requireAbsent(path: string): Promise<void> {
	try {
		await lstat(path);
		throw new Error(`evidence finalizer refuses to replace existing output ${path}`);
	} catch (error) {
		if ((error as NodeJS.ErrnoException).code !== "ENOENT") throw error;
	}
}

async function syncDirectories(paths: readonly string[]): Promise<void> {
	for (const path of new Set(paths.map(dirname))) {
		const handle = await open(path, "r");
		try {
			await handle.sync();
		} finally {
			await handle.close();
		}
	}
}

/**
 * Publish a set of new evidence files without ever replacing prior evidence.
 *
 * Each file is fully fsynced before an atomic hard-link creates its canonical
 * name. If publication or validation fails, every canonical link created by
 * this transaction is removed. Existing canonical files are never unlinked.
 */
export async function commitFilesNoClobber(
	repositoryRoot: string,
	files: readonly NoClobberFile[],
	validate: () => Promise<void>,
): Promise<void> {
	if (files.length === 0) throw new Error("evidence finalizer has no outputs");
	const destinations = files.map(({ path }) => resolve(path));
	if (new Set(destinations).size !== destinations.length) {
		throw new Error("evidence finalizer output paths must be unique");
	}

	for (const path of destinations) {
		await ensureSafeOutputParent(repositoryRoot, path);
		await requireAbsent(path);
	}

	const token = `${process.pid}.${randomUUID()}`;
	const transactionDirectory = resolve(
		repositoryRoot,
		"target/switchable-baseline-inputs/.finalizer-transactions",
		token,
	);
	await ensureSafeOutputParent(
		repositoryRoot,
		resolve(transactionDirectory, ".sentinel"),
	);
	const staged = files.map(({ path, contents }, index) => ({
		contents,
		destination: resolve(path),
		path: resolve(transactionDirectory, `${index}-${basename(path)}.tmp`),
	}));
	const published: PublishedFile[] = [];
	try {
		for (const entry of staged) {
			const handle = await open(entry.path, "wx", 0o644);
			try {
				await handle.writeFile(entry.contents);
				await handle.sync();
			} finally {
				await handle.close();
			}
		}
		await syncDirectories(staged.map(({ path }) => path));
		for (const entry of staged) {
			await link(entry.path, entry.destination);
			published.push(await publishedFile(entry.destination));
		}
		await syncDirectories(published.map(({ path }) => path));
		await validate();
	} catch (error) {
		for (const file of [...published].reverse()) {
			if (await stillOwnsPublishedFile(file)) {
				await unlink(file.path).catch(() => undefined);
			}
		}
		await syncDirectories(published.map(({ path }) => path)).catch(() => undefined);
		throw error;
	} finally {
		await Promise.all(staged.map(({ path }) => unlink(path).catch(() => undefined)));
		await rmdir(transactionDirectory).catch(() => undefined);
	}
}
