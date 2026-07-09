import { describe, expect, test } from "bun:test";
import { mkdirSync, unlinkSync, writeFileSync } from "node:fs";
import {
  lstat,
  mkdir,
  realpath,
  rm,
  symlink,
  unlink,
} from "node:fs/promises";
import { dirname, resolve } from "node:path";
import {
  readPinnedFile,
  requirePinnedFileUnchanged,
  requireRepositorySourceAtCommit,
} from "../scripts/switchable-baseline/gate-evidence/filesystem";
import {
  activateGateZeroGeneration,
  cleanupGateZeroGeneration,
  createGateZeroGeneration,
} from "../scripts/switchable-baseline/generation";
import {
  resolveRestrictedEvidenceInput,
  resolveRestrictedFaroInput,
} from "../scripts/switchable-baseline/filesystem";
import {
  fixtureRoot,
  serverCommit,
} from "./support/gate-evidence-hardening";

describe("Gate 0 activation hardening", () => {
	test("activates one complete directory generation and removes a rejected generation", async () => {
		const commitPath = "attestations/live-collection.sigstore.json";
		const repositoryRoot = await fixtureRoot();
		const generation = await createGateZeroGeneration(repositoryRoot);
		await mkdir(resolve(generation.gateRoot, "attestations"), { recursive: true });
		await Bun.write(resolve(generation.gateRoot, "payload.json"), "{}\n");
		await Bun.write(resolve(generation.gateRoot, commitPath), "{}\n");
		const linked: string[] = [];
		await activateGateZeroGeneration({
			actualRepositoryRoot: repositoryRoot,
			generation,
			commitRelativePath: commitPath,
			testPublicationObserver: (event) => {
				if (event.operation === "file-linked") linked.push(event.relativePath);
			},
			validateBeforeCommit: async () => {
				expect(await Bun.file(resolve(repositoryRoot, "docs/evidence/gate-0", commitPath)).exists())
					.toBeFalse();
			},
		});
		await cleanupGateZeroGeneration(generation);
		expect(linked.at(-1)).toBe(commitPath);
		expect(await Bun.file(resolve(repositoryRoot, "docs/evidence/gate-0", commitPath)).exists())
			.toBeTrue();

		const rejectedRoot = await fixtureRoot();
		const rejected = await createGateZeroGeneration(rejectedRoot);
		await mkdir(resolve(rejected.gateRoot, "attestations"), { recursive: true });
		await Bun.write(resolve(rejected.gateRoot, commitPath), "{}\n");
		await expect(activateGateZeroGeneration({
			actualRepositoryRoot: rejectedRoot,
			generation: rejected,
			commitRelativePath: commitPath,
			validateBeforeCommit: async () => { throw new Error("reject generation"); },
		})).rejects.toThrow("reject generation");
		await cleanupGateZeroGeneration(rejected);
		expect(await Bun.file(resolve(rejectedRoot, "docs/evidence/gate-0", commitPath)).exists())
			.toBeFalse();
	});

	test("never replaces a pre-existing canonical root", async () => {
		const repositoryRoot = await fixtureRoot();
		const generation = await createGateZeroGeneration(repositoryRoot);
		await mkdir(generation.gateRoot, { recursive: true });
		await Bun.write(resolve(generation.gateRoot, "complete.json"), "generated\n");
		const sentinel = resolve(repositoryRoot, "docs/evidence/gate-0/sentinel.txt");
		await mkdir(dirname(sentinel), { recursive: true });
		await Bun.write(sentinel, "existing root\n");
		await expect(activateGateZeroGeneration({
			actualRepositoryRoot: repositoryRoot,
			generation,
			commitRelativePath: "complete.json",
			validateBeforeCommit: async () => undefined,
		})).rejects.toThrow("refuses to replace an existing canonical generation");
		await cleanupGateZeroGeneration(generation);
		expect(await Bun.file(sentinel).text()).toBe("existing root\n");
	});

	test("never replaces subdirectories or files raced into its owned root", async () => {
		for (const existing of ["subdirectory", "file"] as const) {
			const repositoryRoot = await fixtureRoot();
			const generation = await createGateZeroGeneration(repositoryRoot);
			await mkdir(resolve(generation.gateRoot, "nested"), { recursive: true });
			await Bun.write(resolve(generation.gateRoot, "complete.json"), "complete\n");
			await Bun.write(resolve(generation.gateRoot, "payload.json"), "generated\n");
			await Bun.write(resolve(generation.gateRoot, "nested/generated.json"), "generated\n");
			const canonical = resolve(repositoryRoot, "docs/evidence/gate-0");
			const sentinel = existing === "subdirectory"
				? resolve(canonical, "nested/sentinel.txt")
				: resolve(canonical, "payload.json");
			await expect(activateGateZeroGeneration({
				actualRepositoryRoot: repositoryRoot,
				generation,
				commitRelativePath: "complete.json",
				testPublicationObserver: (event) => {
					if (event.operation !== "root-created") return;
					mkdirSync(dirname(sentinel), { recursive: true });
					writeFileSync(sentinel, `${existing}\n`, { flag: "wx" });
				},
				validateBeforeCommit: async () => undefined,
			})).rejects.toThrow();
			await cleanupGateZeroGeneration(generation);
			expect(await Bun.file(sentinel).text()).toBe(`${existing}\n`);
		}
	});

	test("rollback preserves a replacement observed after publication", async () => {
		const repositoryRoot = await fixtureRoot();
		const generation = await createGateZeroGeneration(repositoryRoot);
		await mkdir(generation.gateRoot, { recursive: true });
		await Bun.write(resolve(generation.gateRoot, "payload.json"), "generated\n");
		await Bun.write(resolve(generation.gateRoot, "complete.json"), "complete\n");
		const canonicalPayload = resolve(
			repositoryRoot,
			"docs/evidence/gate-0/payload.json",
		);
		await expect(activateGateZeroGeneration({
			actualRepositoryRoot: repositoryRoot,
			generation,
			commitRelativePath: "complete.json",
			validateBeforeCommit: async () => {
				await unlink(canonicalPayload);
				await Bun.write(canonicalPayload, "replacement\n");
				throw new Error("reject generation");
			},
		})).rejects.toThrow("reject generation");
		await cleanupGateZeroGeneration(generation);
		expect(await Bun.file(canonicalPayload).text()).toBe("replacement\n");
	});

	test("does not commit when a canonical file is replaced during pre-commit validation", async () => {
		const repositoryRoot = await fixtureRoot();
		const generation = await createGateZeroGeneration(repositoryRoot);
		await mkdir(generation.gateRoot, { recursive: true });
		await Bun.write(resolve(generation.gateRoot, "payload.json"), "generated\n");
		await Bun.write(resolve(generation.gateRoot, "complete.json"), "complete\n");
		const canonical = resolve(repositoryRoot, "docs/evidence/gate-0");
		const canonicalPayload = resolve(canonical, "payload.json");
		await expect(activateGateZeroGeneration({
			actualRepositoryRoot: repositoryRoot,
			generation,
			commitRelativePath: "complete.json",
			validateBeforeCommit: async () => {
				await unlink(canonicalPayload);
				await Bun.write(canonicalPayload, "replacement\n");
			},
		})).rejects.toThrow("canonical file changed before activation commit");
		await cleanupGateZeroGeneration(generation);
		expect(await Bun.file(canonicalPayload).text()).toBe("replacement\n");
		expect(await Bun.file(resolve(canonical, "complete.json")).exists()).toBeFalse();
	});

	test("does not commit in-place canonical mutation or an added canonical path", async () => {
		for (const mutation of ["overwrite", "extra-file"] as const) {
			const repositoryRoot = await fixtureRoot();
			const generation = await createGateZeroGeneration(repositoryRoot);
			await mkdir(generation.gateRoot, { recursive: true });
			await Bun.write(resolve(generation.gateRoot, "payload.json"), "generated\n");
			await Bun.write(resolve(generation.gateRoot, "complete.json"), "complete\n");
			const canonical = resolve(repositoryRoot, "docs/evidence/gate-0");
			await expect(activateGateZeroGeneration({
				actualRepositoryRoot: repositoryRoot,
				generation,
				commitRelativePath: "complete.json",
				validateBeforeCommit: async () => {
					if (mutation === "overwrite") {
						await Bun.write(resolve(canonical, "payload.json"), "corrupted\n");
					} else {
						await Bun.write(resolve(canonical, "injected.json"), "injected\n");
					}
				},
			})).rejects.toThrow(
				mutation === "overwrite"
					? "generation changed during pre-commit validation"
					: "canonical tree changed before activation commit",
			);
			await cleanupGateZeroGeneration(generation);
			expect(await Bun.file(resolve(canonical, "complete.json")).exists()).toBeFalse();
		}
	});

	test("does not commit when the unpublished commit source changes during validation", async () => {
		const repositoryRoot = await fixtureRoot();
		const generation = await createGateZeroGeneration(repositoryRoot);
		await mkdir(generation.gateRoot, { recursive: true });
		await Bun.write(resolve(generation.gateRoot, "payload.json"), "generated\n");
		const commitSource = resolve(generation.gateRoot, "complete.json");
		await Bun.write(commitSource, "complete\n");
		await expect(activateGateZeroGeneration({
			actualRepositoryRoot: repositoryRoot,
			generation,
			commitRelativePath: "complete.json",
			validateBeforeCommit: async () => {
				await Bun.write(commitSource, "forged\n");
			},
		})).rejects.toThrow("generation changed during pre-commit validation");
		await cleanupGateZeroGeneration(generation);
		expect(await Bun.file(resolve(repositoryRoot, "docs/evidence/gate-0/complete.json")).exists())
			.toBeFalse();
	});

	test("rolls back a mutation made by the publication observer at commit time", async () => {
		const repositoryRoot = await fixtureRoot();
		const generation = await createGateZeroGeneration(repositoryRoot);
		await mkdir(generation.gateRoot, { recursive: true });
		await Bun.write(resolve(generation.gateRoot, "payload.json"), "generated\n");
		await Bun.write(resolve(generation.gateRoot, "complete.json"), "complete\n");
		const canonical = resolve(repositoryRoot, "docs/evidence/gate-0");
		await expect(activateGateZeroGeneration({
			actualRepositoryRoot: repositoryRoot,
			generation,
			commitRelativePath: "complete.json",
			validateBeforeCommit: async () => undefined,
			testPublicationObserver: (event) => {
				if (event.operation !== "file-linked" || event.relativePath !== "complete.json") return;
				unlinkSync(resolve(canonical, "payload.json"));
				writeFileSync(resolve(canonical, "payload.json"), "observer replacement\n");
			},
		})).rejects.toThrow("canonical file changed while activation was committed");
		await cleanupGateZeroGeneration(generation);
		expect(await Bun.file(resolve(canonical, "payload.json")).text())
			.toBe("observer replacement\n");
		expect(await Bun.file(resolve(canonical, "complete.json")).exists()).toBeFalse();
	});

	test("requires manual stale-lock recovery and preserves a replaced owner lock", async () => {
		const staleRoot = await fixtureRoot();
		const staleLock = resolve(staleRoot, "docs/evidence/.gate-0-finalize.lock");
		await Bun.write(staleLock, '{"pid":999999999,"host":"stale"}\n');
		await expect(createGateZeroGeneration(staleRoot))
			.rejects.toThrow("verify its owner before removing it manually");
		expect(await Bun.file(staleLock).text()).toContain('"host":"stale"');

		const replacedRoot = await fixtureRoot();
		const generation = await createGateZeroGeneration(replacedRoot);
		const lock = resolve(replacedRoot, "docs/evidence/.gate-0-finalize.lock");
		await unlink(lock);
		await Bun.write(lock, "replacement owner\n");
		await expect(cleanupGateZeroGeneration(generation))
			.rejects.toThrow("refuses to remove a replaced activation lock");
		expect(await Bun.file(lock).text()).toBe("replacement owner\n");
		await expect(cleanupGateZeroGeneration(generation)).resolves.toBeUndefined();
		expect(await Bun.file(lock).text()).toBe("replacement owner\n");
	});

	test("consumes lock ownership once across repeated cleanup", async () => {
		const repositoryRoot = await fixtureRoot();
		const generation = await createGateZeroGeneration(repositoryRoot);

		await cleanupGateZeroGeneration(generation);
		await expect(cleanupGateZeroGeneration(generation)).resolves.toBeUndefined();
	});

	test("rejects symlinked evidence and staging ancestors", async () => {
		const repositoryRoot = await fixtureRoot();
		const external = await fixtureRoot();
		await rm(resolve(repositoryRoot, "docs/evidence"), { recursive: true });
		await symlink(resolve(external, "docs/evidence"), resolve(repositoryRoot, "docs/evidence"));
		await expect(createGateZeroGeneration(repositoryRoot))
			.rejects.toThrow("parents must not contain symlinks");

		const stagingRepository = await fixtureRoot();
		const externalTarget = resolve(external, "target");
		const externalInput = resolve(
			externalTarget,
			"switchable-baseline-inputs/capability/live.json",
		);
		await mkdir(dirname(externalInput), { recursive: true });
		await Bun.write(externalInput, "{}\n");
		await symlink(externalTarget, resolve(stagingRepository, "target"));
		await expect(resolveRestrictedEvidenceInput(
			resolve(stagingRepository, "target/switchable-baseline-inputs/capability/live.json"),
			stagingRepository,
			"live capability",
		)).rejects.toThrow("parents must be real directories");

		const faroParent = resolve(stagingRepository, "faro-link");
		await symlink(dirname(externalInput), faroParent);
		await expect(resolveRestrictedFaroInput(
			resolve(faroParent, "live.json"),
			resolve(stagingRepository, "docs/evidence"),
		)).rejects.toThrow("parents must be real directories");
		expect(await realpath(resolve(stagingRepository, "target"))).toBe(externalTarget);
	});

	test("no-follow reads reject symlink swaps", async () => {
		const repositoryRoot = await fixtureRoot();
		const target = resolve(repositoryRoot, "target.json");
		const link = resolve(repositoryRoot, "link.json");
		await Bun.write(target, "{}\n");
		await symlink(target, link);
		expect(() => readPinnedFile(link, "linked evidence")).toThrow("without following symlinks");
	});

	test("rejects a file replaced while asynchronous verification is in progress", async () => {
		const repositoryRoot = await fixtureRoot();
		const path = resolve(repositoryRoot, "verification-subject.json");
		await Bun.write(path, "{\"version\":1}\n");
		const snapshot = readPinnedFile(path, "verification subject");
		const verifier = async () => {
			await Bun.write(path, "{\"version\":2}\n");
		};
		await verifier();
		expect(() => requirePinnedFileUnchanged(path, snapshot, "verification subject"))
			.toThrow("changed while its verification was in progress");
	});

	test("rejects a source swapped while its commit binding is read", async () => {
		const repositoryRoot = await fixtureRoot();
		const sourcePath = "docs/product/source.json";
		const path = resolve(repositoryRoot, sourcePath);
		const original = new TextEncoder().encode("{\"version\":1}\n");
		await mkdir(dirname(path), { recursive: true });
		await Bun.write(path, original);
		await expect(requireRepositorySourceAtCommit(
			repositoryRoot,
			serverCommit,
			sourcePath,
			"swapped source",
			async () => {
				await Bun.write(path, "{\"version\":2}\n");
				return original;
			},
		)).rejects.toThrow("changed while its commit binding was verified");
	});

});
