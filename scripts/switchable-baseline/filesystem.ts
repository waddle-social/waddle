import {
	lstat,
	mkdir,
	realpath,
} from "node:fs/promises";
import { dirname, isAbsolute, relative, resolve, sep } from "node:path";

function isWithin(root: string, candidate: string): boolean {
	const path = relative(root, candidate);
	return path.length === 0 || (!isAbsolute(path) && path !== ".." && !path.startsWith(`..${sep}`));
}

async function existingRealPath(path: string): Promise<string> {
	try {
		return await realpath(path);
	} catch {
		return resolve(path);
	}
}

async function requireRealDirectoryChain(
	rootPath: string,
	directoryPath: string,
	label: string,
): Promise<{ realRoot: string; realDirectory: string }> {
	const root = resolve(rootPath);
	const directory = resolve(directoryPath);
	if (!isWithin(root, directory)) {
		throw new Error(`${label} must remain under its trusted root`);
	}
	let current = root;
	const rootStat = await lstat(current);
	if (rootStat.isSymbolicLink() || !rootStat.isDirectory()) {
		throw new Error(`${label} root must be a real directory`);
	}
	for (const segment of relative(root, directory).split(sep)) {
		if (segment.length === 0) continue;
		current = resolve(current, segment);
		const stat = await lstat(current);
		if (stat.isSymbolicLink() || !stat.isDirectory()) {
			throw new Error(`${label} parents must be real directories`);
		}
	}
	const [realRoot, realDirectory] = await Promise.all([
		realpath(root),
		realpath(directory),
	]);
	if (!isWithin(realRoot, realDirectory)) {
		throw new Error(`${label} must remain under its trusted root`);
	}
	return { realRoot, realDirectory };
}

async function requireAbsoluteDirectoryChain(path: string, label: string): Promise<void> {
	const absolute = resolve(path);
	let current = absolute.startsWith(sep) ? sep : absolute.slice(0, 3);
	for (const segment of absolute.split(sep).filter(Boolean)) {
		current = resolve(current, segment);
		const stat = await lstat(current);
		if (stat.isSymbolicLink() || !stat.isDirectory()) {
			throw new Error(`${label} parents must be real directories`);
		}
	}
}

export async function resolveRestrictedFaroInput(
	input: string,
	evidenceRoot: string,
): Promise<string> {
	const inputPath = resolve(input);
	let inputStat;
	try {
		inputStat = await lstat(inputPath);
	} catch {
		throw new Error("restricted Faro aggregate input must exist");
	}
	if (inputStat.isSymbolicLink()) {
		throw new Error("restricted Faro aggregate input must not be a symlink");
	}
	if (!inputStat.isFile()) {
		throw new Error("restricted Faro aggregate input must be a regular file");
	}
	await requireAbsoluteDirectoryChain(dirname(inputPath), "restricted Faro aggregate input");

	const resolvedEvidenceRoot = resolve(evidenceRoot);
	const realInput = await realpath(inputPath);
	const realEvidenceRoot = await existingRealPath(resolvedEvidenceRoot);
	if (
		isWithin(resolvedEvidenceRoot, inputPath)
		|| isWithin(realEvidenceRoot, realInput)
	) {
		throw new Error("restricted Faro aggregate input must remain outside docs/evidence");
	}
	return realInput;
}

export async function resolveRestrictedEvidenceInput(
	input: string,
	repositoryRoot: string,
	label: string,
): Promise<string> {
	const inputPath = resolve(input);
	const stagingRoot = resolve(
		repositoryRoot,
		"target/switchable-baseline-inputs",
	);
	let inputStat;
	try {
		inputStat = await lstat(inputPath);
	} catch {
		throw new Error(`staged ${label} input must exist`);
	}
	if (inputStat.isSymbolicLink()) {
		throw new Error(`staged ${label} input must not be a symlink`);
	}
	if (!inputStat.isFile()) {
		throw new Error(`staged ${label} input must be a regular file`);
	}
	try {
		await requireRealDirectoryChain(
			repositoryRoot,
			stagingRoot,
			"switchable baseline staging",
		);
	} catch (error) {
		if ((error as NodeJS.ErrnoException).code === "ENOENT") {
			throw new Error("switchable baseline staging root must exist");
		}
		throw error;
	}
	const relativePath = relative(stagingRoot, inputPath);
	if (
		relativePath.length === 0
		|| isAbsolute(relativePath)
		|| relativePath === ".."
		|| relativePath.startsWith(`..${sep}`)
	) {
		throw new Error(
			`staged ${label} input must be under target/switchable-baseline-inputs`,
		);
	}
	const { realRoot } = await requireRealDirectoryChain(
		stagingRoot,
		dirname(inputPath),
		`staged ${label} input`,
	);
	const realInput = await realpath(inputPath);
	if (!isWithin(realRoot, realInput) || realRoot === realInput) {
		throw new Error(
			`staged ${label} input must remain under target/switchable-baseline-inputs`,
		);
	}
	return realInput;
}

export async function ensureSafeOutputParent(
	repositoryRoot: string,
	outputPath: string,
): Promise<void> {
	const root = resolve(repositoryRoot);
	const destination = resolve(outputPath);
	if (!isWithin(root, destination) || destination === root) {
		throw new Error("evidence output must remain inside the repository");
	}
	await requireAbsoluteDirectoryChain(dirname(root), "evidence repository");
	const rootStat = await lstat(root);
	if (rootStat.isSymbolicLink() || !rootStat.isDirectory()) {
		throw new Error("evidence repository root must be a real directory");
	}

	const parent = dirname(destination);
	let current = root;
	for (const segment of relative(root, parent).split(sep)) {
		if (segment.length === 0) continue;
		current = resolve(current, segment);
		try {
			const stat = await lstat(current);
			if (stat.isSymbolicLink()) {
				throw new Error("evidence output parents must not contain symlinks");
			}
			if (!stat.isDirectory()) {
				throw new Error("evidence output parents must be directories");
			}
		} catch (error) {
			if (error instanceof Error && error.message.startsWith("evidence")) throw error;
			if ((error as NodeJS.ErrnoException).code !== "ENOENT") throw error;
			await mkdir(current).catch(async (mkdirError: unknown) => {
				if ((mkdirError as NodeJS.ErrnoException).code !== "EEXIST") throw mkdirError;
			});
			const created = await lstat(current);
			if (created.isSymbolicLink() || !created.isDirectory()) {
				throw new Error("evidence output parents must be real directories");
			}
		}
	}

	const [realRoot, realParent] = await Promise.all([
		realpath(root),
		realpath(parent),
	]);
	if (!isWithin(realRoot, realParent)) {
		throw new Error("evidence output parent must remain inside the repository");
	}
	try {
		const destinationStat = await lstat(destination);
		if (destinationStat.isSymbolicLink()) {
			throw new Error("evidence output must not be a symlink");
		}
		if (!destinationStat.isFile()) {
			throw new Error("evidence output must be a regular file");
		}
	} catch (error) {
		if (error instanceof Error && error.message.startsWith("evidence")) throw error;
		if ((error as NodeJS.ErrnoException).code !== "ENOENT") throw error;
	}
}
