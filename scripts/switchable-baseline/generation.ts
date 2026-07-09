import { createHash, randomUUID } from "node:crypto";
import {
	closeSync,
	constants,
	fstatSync,
	fsyncSync,
	linkSync,
	lstatSync,
	mkdirSync,
	openSync,
	readFileSync,
	readdirSync,
	rmdirSync,
	rmSync,
	unlinkSync,
	writeFileSync,
} from "node:fs";
import { mkdir, open, rm } from "node:fs/promises";
import { hostname } from "node:os";
import { dirname, relative, resolve, sep } from "node:path";
import { ensureSafeOutputParent } from "./filesystem";

const transactionPrefix = ".gate-0-finalize-";
const lockName = ".gate-0-finalize.lock";
const staleAfterMs = 6 * 60 * 60 * 1_000;

export interface GateZeroGeneration {
	transactionRoot: string;
	repositoryRoot: string;
	gateRoot: string;
	releaseLock: () => void;
}

type GateZeroPublicationEvent = {
	operation: "root-created" | "directory-created" | "file-linked";
	relativePath: string;
};

function syncDirectorySync(path: string): void {
	const descriptor = openSync(path, constants.O_RDONLY | constants.O_NOFOLLOW);
	try {
		fsyncSync(descriptor);
	} finally {
		closeSync(descriptor);
	}
}

function recoverLock(evidenceRoot: string): void {
	const lockPath = resolve(evidenceRoot, lockName);
	let descriptor: number;
	try {
		descriptor = openSync(lockPath, constants.O_RDONLY | constants.O_NOFOLLOW);
	} catch (error) {
		if ((error as NodeJS.ErrnoException).code === "ENOENT") return;
		throw new Error("Gate 0 finalizer lock must be a regular non-symlink file", {
			cause: error,
		});
	}
	try {
		if (!fstatSync(descriptor).isFile()) {
			throw new Error("Gate 0 finalizer lock must be a regular file");
		}
	} finally {
		closeSync(descriptor);
	}
	// POSIX exposes no portable conditional unlink-by-inode. Automatically
	// deleting a path judged stale would therefore let a racing recovery remove
	// a new owner's lock. Stale locks require explicit operator recovery.
	throw new Error("a Gate 0 finalizer lock exists; verify its owner before removing it manually");
}

function acquireOwnedLock(lockPath: string, evidenceRoot: string): OwnedLock {
	const descriptor = openSync(
		lockPath,
		constants.O_WRONLY | constants.O_CREAT | constants.O_EXCL | constants.O_NOFOLLOW,
		0o600,
	);
	try {
		writeFileSync(descriptor, JSON.stringify({ pid: process.pid, host: hostname() }) + "\n");
		fsyncSync(descriptor);
		const stat = fstatSync(descriptor);
		if (!stat.isFile()) throw new Error("Gate 0 finalizer lock must be a regular file");
		syncDirectorySync(evidenceRoot);
		return {
			path: lockPath,
			device: stat.dev,
			inode: stat.ino,
			descriptor,
		};
	} catch (error) {
		try {
			const stat = fstatSync(descriptor);
			const created = { path: lockPath, device: stat.dev, inode: stat.ino };
			if (sameIdentity(created)) {
				unlinkSync(lockPath);
				syncDirectorySync(evidenceRoot);
			}
		} catch {
			// Preserve a path that cannot be proven to be the lock created above.
		} finally {
			closeSync(descriptor);
		}
		throw error;
	}
}

function releaseOwnedLock(lock: OwnedLock, evidenceRoot: string): void {
	try {
		const descriptorStat = fstatSync(lock.descriptor);
		if (
			descriptorStat.dev !== lock.device
			|| descriptorStat.ino !== lock.inode
			|| !sameIdentity(lock)
		) {
			throw new Error("Gate 0 finalizer refuses to remove a replaced activation lock");
		}
		unlinkSync(lock.path);
		syncDirectorySync(evidenceRoot);
	} finally {
		closeSync(lock.descriptor);
	}
}

function recoverStaleTransactions(evidenceRoot: string): void {
	for (const entry of readdirSync(evidenceRoot, { withFileTypes: true })) {
		if (!entry.name.startsWith(transactionPrefix)) continue;
		const path = resolve(evidenceRoot, entry.name);
		const stat = lstatSync(path);
		if (stat.isSymbolicLink() || !stat.isDirectory()) {
			throw new Error("Gate 0 finalizer staging entries must be real directories");
		}
		if (Date.now() - stat.mtimeMs >= staleAfterMs) {
			rmSync(path, { recursive: true });
			syncDirectorySync(evidenceRoot);
		}
	}
}

export async function createGateZeroGeneration(
	repositoryRoot: string,
): Promise<GateZeroGeneration> {
	const evidenceRoot = resolve(repositoryRoot, "docs/evidence");
	await ensureSafeOutputParent(repositoryRoot, resolve(evidenceRoot, ".generation-sentinel"));
	recoverLock(evidenceRoot);
	recoverStaleTransactions(evidenceRoot);
	const lockPath = resolve(evidenceRoot, lockName);
	const lock = acquireOwnedLock(lockPath, evidenceRoot);
	let lockConsumed = false;
	const releaseLock = (): void => {
		if (lockConsumed) return;
		lockConsumed = true;
		releaseOwnedLock(lock, evidenceRoot);
	};
	const transactionRoot = resolve(evidenceRoot, `${transactionPrefix}${randomUUID()}`);
	try {
		await mkdir(transactionRoot, { mode: 0o700 });
		const isolatedRepository = resolve(transactionRoot, "repository");
		await mkdir(isolatedRepository, { mode: 0o700 });
		return {
			transactionRoot,
			repositoryRoot: isolatedRepository,
			gateRoot: resolve(isolatedRepository, "docs/evidence/gate-0"),
			releaseLock,
		};
	} catch (error) {
		releaseLock();
		throw error;
	}
}

async function syncDirectory(path: string): Promise<void> {
	const handle = await open(path, constants.O_RDONLY | constants.O_NOFOLLOW);
	try {
		await handle.sync();
	} finally {
		await handle.close();
	}
}

async function syncTree(directory: string): Promise<void> {
	const directories: string[] = [];
	const visit = async (path: string): Promise<void> => {
		directories.push(path);
		for (const entry of readdirSync(path, { withFileTypes: true })) {
			const child = resolve(path, entry.name);
			const stat = lstatSync(child);
			if (stat.isSymbolicLink()) throw new Error("Gate 0 generation must not contain symlinks");
			if (stat.isDirectory()) {
				await visit(child);
				continue;
			}
			if (!stat.isFile()) throw new Error("Gate 0 generation must contain only regular files");
			const handle = await open(child, constants.O_RDONLY | constants.O_NOFOLLOW);
			try {
				await handle.sync();
			} finally {
				await handle.close();
			}
		}
	};
	await visit(directory);
	for (const path of directories.reverse()) await syncDirectory(path);
}

export async function activateGateZeroGeneration(input: {
	actualRepositoryRoot: string;
	generation: GateZeroGeneration;
	commitRelativePath: string;
	validateBeforeCommit: () => Promise<void>;
	/** Test seam for deterministic publication-race coverage. */
	testPublicationObserver?: (event: GateZeroPublicationEvent) => void;
}): Promise<void> {
	const evidenceRoot = resolve(input.actualRepositoryRoot, "docs/evidence");
	const canonical = resolve(evidenceRoot, "gate-0");
	await ensureSafeOutputParent(
		input.actualRepositoryRoot,
		resolve(evidenceRoot, ".generation-sentinel"),
	);
	await syncTree(input.generation.gateRoot);
	await syncDirectory(resolve(input.generation.repositoryRoot, "docs/evidence"));
	await syncDirectory(evidenceRoot);
	await publishGenerationNoClobber({
		sourceRoot: input.generation.gateRoot,
		destinationRoot: canonical,
		commitRelativePath: input.commitRelativePath,
		validateBeforeCommit: input.validateBeforeCommit,
		testObserver: input.testPublicationObserver,
	});
	await syncDirectory(evidenceRoot);
}

interface FileIdentity {
	path: string;
	device: bigint | number;
	inode: bigint | number;
}

interface OwnedLock extends FileIdentity {
	descriptor: number;
}

interface FileSnapshot extends FileIdentity {
	sha256: string;
}

function identity(path: string): FileIdentity {
	const stat = lstatSync(path);
	return { path, device: stat.dev, inode: stat.ino };
}

function sameIdentity(entry: FileIdentity): boolean {
	try {
		const stat = lstatSync(entry.path);
		return stat.dev === entry.device && stat.ino === entry.inode;
	} catch {
		return false;
	}
}

function snapshotFile(path: string): FileSnapshot {
	const descriptor = openSync(path, constants.O_RDONLY | constants.O_NOFOLLOW);
	try {
		const stat = fstatSync(descriptor);
		if (!stat.isFile()) throw new Error("Gate 0 generation must contain only regular files");
		const bytes = readFileSync(descriptor);
		return {
			path,
			device: stat.dev,
			inode: stat.ino,
			sha256: createHash("sha256").update(bytes).digest("hex"),
		};
	} finally {
		closeSync(descriptor);
	}
}

function sameSnapshot(entry: FileSnapshot): boolean {
	try {
		const current = snapshotFile(entry.path);
		return current.device === entry.device
			&& current.inode === entry.inode
			&& current.sha256 === entry.sha256;
	} catch {
		return false;
	}
}

function samePaths(actual: readonly string[], expected: readonly string[]): boolean {
	return JSON.stringify(actual) === JSON.stringify(expected);
}

function generationTree(root: string): { directories: string[]; files: string[] } {
	const directories: string[] = [];
	const files: string[] = [];
	const visit = (directory: string): void => {
		for (const entry of readdirSync(directory, { withFileTypes: true })) {
			const path = resolve(directory, entry.name);
			const stat = lstatSync(path);
			if (stat.isSymbolicLink()) {
				throw new Error("Gate 0 generation must not contain symlinks");
			}
			const repositoryPath = relative(root, path).split(sep).join("/");
			if (entry.isDirectory() && stat.isDirectory()) {
				directories.push(repositoryPath);
				visit(path);
			} else if (entry.isFile() && stat.isFile()) {
				files.push(repositoryPath);
			} else {
				throw new Error("Gate 0 generation must contain only regular files");
			}
		}
	};
	visit(root);
	return {
		directories: directories.sort((left, right) => left.split("/").length - right.split("/").length),
		files: files.sort(),
	};
}

function gateZeroPublicationOrder(
	files: readonly string[],
	commitRelativePath: string,
): string[] {
	if (!files.includes(commitRelativePath)) {
		throw new Error("Gate 0 generation is missing its final commit artifact");
	}
	return [
		...files.filter((path) => path !== commitRelativePath),
		commitRelativePath,
	];
}

async function publishGenerationNoClobber(input: {
	sourceRoot: string;
	destinationRoot: string;
	commitRelativePath: string;
	validateBeforeCommit: () => Promise<void>;
	testObserver?: (event: GateZeroPublicationEvent) => void;
}): Promise<void> {
	const tree = generationTree(input.sourceRoot);
	const publicationOrder = gateZeroPublicationOrder(tree.files, input.commitRelativePath);
	const sourceSnapshots = new Map(publicationOrder.map((repositoryPath) => [
		repositoryPath,
		snapshotFile(resolve(input.sourceRoot, repositoryPath)),
	]));
	const createdDirectories: FileIdentity[] = [];
	const publishedFiles: FileIdentity[] = [];
	try {
		try {
			mkdirSync(input.destinationRoot, { mode: 0o700 });
		} catch (error) {
			if ((error as NodeJS.ErrnoException).code === "EEXIST") {
				throw new Error("Gate 0 finalizer refuses to replace an existing canonical generation");
			}
			throw error;
		}
		createdDirectories.push(identity(input.destinationRoot));
		input.testObserver?.({ operation: "root-created", relativePath: "." });
		for (const repositoryPath of tree.directories) {
			const destination = resolve(input.destinationRoot, repositoryPath);
			mkdirSync(destination, { mode: 0o700 });
			createdDirectories.push(identity(destination));
			input.testObserver?.({
				operation: "directory-created",
				relativePath: repositoryPath,
			});
		}

		const publish = (repositoryPath: string): void => {
			const source = resolve(input.sourceRoot, repositoryPath);
			const destination = resolve(input.destinationRoot, repositoryPath);
			linkSync(source, destination);
			publishedFiles.push(identity(destination));
			input.testObserver?.({ operation: "file-linked", relativePath: repositoryPath });
		};
		for (const repositoryPath of publicationOrder.slice(0, -1)) {
			publish(repositoryPath);
		}
		for (const directory of [...createdDirectories].reverse()) {
			syncDirectorySync(directory.path);
		}
		// The canonical root is deliberately incomplete until validation succeeds.
		// Its final attestation link is the durable commit record, so it must never
		// become visible before the fully linked generation has passed validation.
		await input.validateBeforeCommit();
		for (const snapshot of sourceSnapshots.values()) {
			if (!sameSnapshot(snapshot)) {
				throw new Error("Gate 0 generation changed during pre-commit validation");
			}
		}
		const canonicalTree = generationTree(input.destinationRoot);
		if (
			!samePaths(canonicalTree.directories, tree.directories)
			|| !samePaths(canonicalTree.files, publicationOrder.slice(0, -1))
		) {
			throw new Error("Gate 0 canonical tree changed before activation commit");
		}
		for (const file of publishedFiles) {
			if (!sameIdentity(file)) {
				throw new Error("Gate 0 canonical file changed before activation commit");
			}
		}
		for (const directory of createdDirectories) {
			if (!sameIdentity(directory)) {
				throw new Error("Gate 0 canonical directory changed before activation commit");
			}
		}
		const commitPath = publicationOrder.at(-1);
		if (!commitPath) throw new Error("Gate 0 publication order is empty");
		publish(commitPath);
		const committedTree = generationTree(input.destinationRoot);
		if (
			!samePaths(committedTree.directories, tree.directories)
			|| !samePaths(committedTree.files, tree.files)
		) {
			throw new Error("Gate 0 canonical tree changed while activation was committed");
		}
		for (const snapshot of sourceSnapshots.values()) {
			if (!sameSnapshot(snapshot)) {
				throw new Error("Gate 0 generation changed while activation was committed");
			}
		}
		for (const file of publishedFiles) {
			if (!sameIdentity(file)) {
				throw new Error("Gate 0 canonical file changed while activation was committed");
			}
		}
		for (const directory of createdDirectories) {
			if (!sameIdentity(directory)) {
				throw new Error("Gate 0 canonical directory changed while activation was committed");
			}
		}
		syncDirectorySync(dirname(resolve(input.destinationRoot, input.commitRelativePath)));
		syncDirectorySync(input.destinationRoot);
		syncDirectorySync(dirname(input.destinationRoot));
	} catch (error) {
		// Cooperating finalizers are serialized by the activation lock. These inode checks
		// additionally preserve a replacement already observed during non-malicious
		// contention; POSIX has no portable conditional unlink, so hostile mutation
		// between the identity check and unlink remains outside this rollback guarantee.
		for (const file of publishedFiles.reverse()) {
			if (sameIdentity(file)) {
				try {
					unlinkSync(file.path);
				} catch {
					// A raced replacement or non-file is never removed by rollback.
				}
			}
		}
		for (const directory of createdDirectories.reverse()) {
			if (sameIdentity(directory)) {
				try {
					rmdirSync(directory.path);
				} catch {
					// Preserve non-empty or raced directories rather than clobbering them.
				}
			}
		}
		throw error;
	}
}

export async function cleanupGateZeroGeneration(generation: GateZeroGeneration): Promise<void> {
	try {
		await rm(generation.transactionRoot, { recursive: true, force: true });
	} finally {
		generation.releaseLock();
	}
}
