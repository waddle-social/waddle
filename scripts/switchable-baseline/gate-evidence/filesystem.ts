import { createHash } from "node:crypto";
import {
  closeSync,
  constants,
  existsSync,
  fstatSync,
  lstatSync,
  openSync,
  readFileSync,
  readdirSync,
  realpathSync,
} from "node:fs";
import { isAbsolute, relative, resolve, sep } from "node:path";
import { parseJsonDocument } from "../json";
import { canonicalGateZeroPaths, fail } from "./common";

export type RepositorySourceAtCommitReader = (
  repositoryRoot: string,
  commit: string,
  repositoryPath: string,
) => Promise<Uint8Array>;

const evidencePathPattern = new RegExp(
  "^docs/evidence/[A-Za-z0-9][A-Za-z0-9._/-]*$",
);

export function requireEvidencePath(path: string, label: string): void {
  const segments = path.split("/");
  if (
    !evidencePathPattern.test(path)
    || segments.some(
      (segment) =>
        segment.length === 0 || segment === "." || segment === "..",
    )
  ) {
    fail(label + " must be a repository-relative path under docs/evidence");
  }
}

function assertNoSymlinks(
  root: string,
  candidate: string,
  label: string,
): void {
  if (lstatSync(root).isSymbolicLink()) {
    fail(label + " trust root must not be a symlink");
  }
  const relativePath = relative(root, candidate);
  let current = root;
  for (const segment of relativePath.split(sep)) {
    if (segment.length === 0) continue;
    current = resolve(current, segment);
    if (lstatSync(current).isSymbolicLink()) {
      fail(label + " must not contain symlinks");
    }
  }
}

export function resolveTrustedRepositoryFile(
  repositoryRoot: string,
  path: string,
  trustedPrefix: string,
  label: string,
): string {
  if (isAbsolute(path) || path.includes(String.fromCharCode(92))) {
    fail(label + " must be repository-relative");
  }
  const segments = path.split("/");
  if (
    !path.startsWith(trustedPrefix + "/")
    || segments.some(
      (segment) =>
        segment.length === 0 || segment === "." || segment === "..",
    )
  ) {
    fail(label + " must resolve beneath " + trustedPrefix);
  }
  const root = resolve(repositoryRoot);
  const trustRoot = resolve(root, trustedPrefix);
  const candidate = resolve(root, path);
  const candidateRelative = relative(trustRoot, candidate);
  if (
    candidateRelative.length === 0
    || candidateRelative === ".."
    || candidateRelative.startsWith(".." + sep)
    || isAbsolute(candidateRelative)
  ) {
    fail(label + " must resolve beneath " + trustedPrefix);
  }
  if (!existsSync(trustRoot)) fail(trustedPrefix + " does not exist");
  if (!existsSync(candidate)) fail(label + " does not exist: " + path);
  assertNoSymlinks(root, trustRoot, label + " trust root");
  assertNoSymlinks(trustRoot, candidate, label);
  if (!lstatSync(candidate).isFile()) {
    fail(label + " must be a regular file");
  }

  const realRoot = realpathSync(root);
  const realTrustRoot = realpathSync(trustRoot);
  const rootRelative = relative(realRoot, realTrustRoot);
  if (
    rootRelative === ".."
    || rootRelative.startsWith(".." + sep)
    || isAbsolute(rootRelative)
  ) {
    fail(label + " trust root must remain inside the repository");
  }
  const realCandidate = realpathSync(candidate);
  const realRelative = relative(realTrustRoot, realCandidate);
  if (
    realRelative.length === 0
    || realRelative === ".."
    || realRelative.startsWith(".." + sep)
    || isAbsolute(realRelative)
  ) {
    fail(label + " must not escape " + trustedPrefix);
  }
  return realCandidate;
}

export function resolveTrustedEvidenceFile(
  repositoryRoot: string,
  path: string,
  label: string,
  extension?: string,
): string {
  requireEvidencePath(path, label);
  if (extension && !path.endsWith(extension)) {
    fail(label + " must name a " + extension + " file");
  }
  return resolveTrustedRepositoryFile(
    repositoryRoot,
    path,
    "docs/evidence",
    label,
  );
}

export function fileSha256(path: string): string {
  return readPinnedFile(path, "evidence file").sha256;
}

export interface PinnedFileSnapshot {
  bytes: Buffer;
  sha256: string;
  device: bigint | number;
  inode: bigint | number;
}

function samePinnedFile(
  left: ReturnType<typeof fstatSync>,
  right: ReturnType<typeof fstatSync>,
): boolean {
  return left.dev === right.dev
    && left.ino === right.ino
    && left.mode === right.mode
    && left.size === right.size
    && left.mtimeMs === right.mtimeMs
    && left.ctimeMs === right.ctimeMs;
}

/** Read one regular file through a no-follow descriptor and pin hash to bytes. */
export function readPinnedFile(path: string, label: string): PinnedFileSnapshot {
  let descriptor: number | undefined;
  try {
    descriptor = openSync(path, constants.O_RDONLY | constants.O_NOFOLLOW);
    const before = fstatSync(descriptor);
    if (!before.isFile()) fail(label + " must be a regular file");
    const bytes = readFileSync(descriptor);
    const after = fstatSync(descriptor);
    if (!samePinnedFile(before, after) || bytes.length !== after.size) {
      fail(label + " changed while it was being read");
    }
    return {
      bytes,
      sha256: createHash("sha256").update(bytes).digest("hex"),
      device: before.dev,
      inode: before.ino,
    };
  } catch (error) {
    if (error instanceof Error && error.message.startsWith("gate evidence:")) {
      throw error;
    }
    fail(label + " could not be opened without following symlinks");
  } finally {
    if (descriptor !== undefined) closeSync(descriptor);
  }
}

/** Prove a pathname still names the exact regular file bytes previously read. */
export function requirePinnedFileUnchanged(
  path: string,
  snapshot: PinnedFileSnapshot,
  label: string,
): void {
  const current = readPinnedFile(path, label);
  if (
    current.device !== snapshot.device
    || current.inode !== snapshot.inode
    || current.sha256 !== snapshot.sha256
  ) fail(label + " changed while its verification was in progress");
}

export function readTrustedJsonSnapshot(
  repositoryRoot: string,
  path: string,
  trustedPrefix: string,
  label: string,
  extension = ".json",
  expectedSha256?: string,
): PinnedFileSnapshot & { value: unknown; path: string } {
  if (extension && !path.endsWith(extension)) {
    fail(label + " must name a " + extension + " file");
  }
  const trustedPath = resolveTrustedRepositoryFile(
    repositoryRoot,
    path,
    trustedPrefix,
    label,
  );
  const snapshot = readPinnedFile(trustedPath, label);
  if (expectedSha256 !== undefined && snapshot.sha256 !== expectedSha256) {
    fail(label + " SHA-256 does not match the expected bytes");
  }
  const currentPath = resolveTrustedRepositoryFile(
    repositoryRoot,
    path,
    trustedPrefix,
    label,
  );
  const current = lstatSync(currentPath);
  if (current.dev !== snapshot.device || current.ino !== snapshot.inode) {
    fail(label + " changed while it was being read");
  }
  let value: unknown;
  try {
    value = parseJsonDocument(snapshot.bytes.toString("utf8"), label);
  } catch (error) {
    fail(error instanceof Error ? error.message : label + " does not contain valid JSON");
  }
  return { ...snapshot, value, path: trustedPath };
}

export async function readRepositorySourceAtCommit(
  repositoryRoot: string,
  commit: string,
  repositoryPath: string,
): Promise<Uint8Array> {
  const process = Bun.spawn(["git", "show", commit + ":" + repositoryPath], {
    cwd: repositoryRoot,
    stdout: "pipe",
    stderr: "ignore",
  });
  const [exitCode, contents] = await Promise.all([
    process.exited,
    new Response(process.stdout).arrayBuffer(),
  ]);
  if (exitCode !== 0) {
    fail("could not read " + repositoryPath + " at the asserted evidence commit");
  }
  return new Uint8Array(contents);
}

export async function requireRepositorySourceAtCommit(
  repositoryRoot: string,
  commit: string,
  repositoryPath: string,
  label: string,
  reader: RepositorySourceAtCommitReader = readRepositorySourceAtCommit,
	expectedSnapshot?: PinnedFileSnapshot,
): Promise<void> {
  const currentPath = resolveTrustedRepositoryFile(
    repositoryRoot,
    repositoryPath,
    repositoryPath.split("/", 1)[0],
    label,
  );
	const currentContents = expectedSnapshot ?? readPinnedFile(currentPath, label);
  let committedContents: Uint8Array;
  try {
    committedContents = await reader(repositoryRoot, commit, repositoryPath);
  } catch (error) {
    if (error instanceof Error && error.message.startsWith("gate evidence:")) throw error;
    fail("could not read " + repositoryPath + " at the asserted evidence commit");
  }
	const currentPathAfterRead = resolveTrustedRepositoryFile(
		repositoryRoot,
		repositoryPath,
		repositoryPath.split("/", 1)[0],
		label,
	);
	const currentAfterRead = readPinnedFile(currentPathAfterRead, label);
	if (
		currentAfterRead.device !== currentContents.device
		|| currentAfterRead.inode !== currentContents.inode
		|| currentAfterRead.sha256 !== currentContents.sha256
	) fail(label + " changed while its commit binding was verified");
  if (!currentContents.bytes.equals(Buffer.from(committedContents))) {
    fail(label + " bytes must match the asserted evidence commit");
  }
}

export function readJsonFile(path: string, label: string): unknown {
  const snapshot = readPinnedFile(path, label);
  try {
    return parseJsonDocument(snapshot.bytes.toString("utf8"), label);
  } catch (error) {
    fail(error instanceof Error ? error.message : label + " does not contain valid JSON");
  }
}

export function requireSealedGateZeroEvidenceDirectory(
  repositoryRoot: string,
  additionalCanonicalPaths: readonly string[] = [],
): void {
  const root = resolve(repositoryRoot);
  const gateRoot = resolve(root, "docs/evidence/gate-0");
  if (!existsSync(gateRoot)) fail("complete Gate 0 evidence directory does not exist");
  if (lstatSync(gateRoot).isSymbolicLink()) {
    fail("complete Gate 0 evidence directory must not be a symlink");
  }
  const expectedFiles = new Set([
    ...canonicalGateZeroPaths,
    ...additionalCanonicalPaths,
  ]);
  const expectedDirectories = new Set<string>(["docs/evidence/gate-0"]);
  for (const path of expectedFiles) {
    let parent = path.slice(0, path.lastIndexOf("/"));
    while (parent.startsWith("docs/evidence/gate-0")) {
      expectedDirectories.add(parent);
      if (parent === "docs/evidence/gate-0") break;
      parent = parent.slice(0, parent.lastIndexOf("/"));
    }
  }

  const actualFiles = new Set<string>();
  const visit = (directory: string): void => {
    for (const entry of readdirSync(directory, { withFileTypes: true })) {
      const absolute = resolve(directory, entry.name);
      const repositoryPath = relative(root, absolute).split(sep).join("/");
      const stat = lstatSync(absolute);
      if (stat.isSymbolicLink()) {
        fail("complete Gate 0 evidence must not contain symlinks: " + repositoryPath);
      }
      if (stat.isDirectory()) {
        if (!expectedDirectories.has(repositoryPath)) {
          fail("complete Gate 0 evidence contains a noncanonical directory: " + repositoryPath);
        }
        visit(absolute);
        continue;
      }
      if (!stat.isFile()) {
        fail("complete Gate 0 evidence contains a non-file entry: " + repositoryPath);
      }
      actualFiles.add(repositoryPath);
    }
  };
  visit(gateRoot);

  const actual = [...actualFiles].sort();
  const expected = [...expectedFiles].sort();
  if (JSON.stringify(actual) !== JSON.stringify(expected)) {
    const unexpected = actual.filter((path) => !expectedFiles.has(path));
    const missing = expected.filter((path) => !actualFiles.has(path));
    fail(
      "complete Gate 0 evidence directory must contain only referenced canonical files"
        + (unexpected.length > 0 ? "; unexpected: " + unexpected.join(", ") : "")
        + (missing.length > 0 ? "; missing: " + missing.join(", ") : ""),
    );
  }
}
