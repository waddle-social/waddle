import { describe, expect, test } from "bun:test";
import {
  resolveCommitIdentity,
  resolveCommitSha,
} from "../scripts/resolve-commit-sha.mjs";

const FULL_SHA = "0123456789abcdef0123456789abcdef01234567";
const OTHER_SHA = "89abcdef0123456789abcdef0123456789abcdef";

function gitExecutor(options: { head?: string; status?: string } = {}) {
  const head = options.head ?? FULL_SHA;
  return (command: string) => {
    switch (command) {
      case "git rev-parse --is-inside-work-tree":
        return Buffer.from("true\n");
      case "git rev-parse HEAD":
        return Buffer.from(`${head}\n`);
      case "git status --porcelain --untracked-files=normal":
        return Buffer.from(options.status ?? "");
      default:
        throw new Error(`unexpected command: ${command}`);
    }
  };
}

const noGit = () => {
  throw new Error("not a git checkout");
};

describe("resolveCommitIdentity", () => {
  test("uses the exact full git object name", () => {
    const identity = resolveCommitIdentity({
      env: {},
      execSync: gitExecutor(),
    });

    expect(identity).toEqual({
      commitSha: FULL_SHA,
      source: { kind: "git", commitSha: FULL_SHA },
    });
    expect(resolveCommitSha({ env: {}, execSync: gitExecutor() })).toBe(FULL_SHA);
  });

  test("requires every supplied deployment SHA to be full and match git HEAD", () => {
    expect(() => resolveCommitIdentity({
      env: { WADDLE_GIT_SHA: FULL_SHA.slice(0, 12) },
      execSync: gitExecutor(),
    })).toThrow("WADDLE_GIT_SHA must be a full 40-character commit SHA");

    expect(() => resolveCommitIdentity({
      env: { GITHUB_SHA: OTHER_SHA },
      execSync: gitExecutor(),
    })).toThrow(`GITHUB_SHA does not match git HEAD ${FULL_SHA}`);
  });

  test("fails immutable builds when the git worktree is dirty", () => {
    expect(() => resolveCommitIdentity({
      env: { PUBLIC_FARO_URL: "https://faro.example/collect" },
      execSync: gitExecutor({ status: " M src/app.ts\n" }),
    })).toThrow("production/Faro builds require a clean git worktree");
  });

  test("allows ignored generated outputs because git status remains clean", () => {
    const identity = resolveCommitIdentity({
      env: { CUENV_ENVIRONMENT: "production" },
      execSync: gitExecutor({ status: "" }),
    });
    expect(identity.commitSha).toBe(FULL_SHA);
  });

  test("rejects source archives that try to self-attest a commit", () => {
    expect(() => resolveCommitIdentity({
      env: {
        WADDLE_GIT_SHA: FULL_SHA,
        WADDLE_SOURCE_ARCHIVE_URL: `https://github.com/waddle-social/waddle/archive/${FULL_SHA}.tar.gz`,
        WADDLE_SOURCE_ARCHIVE_SHA256: "ab".repeat(32),
      },
      execSync: noGit,
    })).toThrow("cannot attest a source archive");
  });

  test("returns unknown only for a non-immutable source with no claimed revision", () => {
    expect(resolveCommitIdentity({ env: {}, execSync: noGit })).toEqual({
      commitSha: "unknown",
      source: { kind: "unknown", commitSha: "unknown" },
    });
    expect(() => resolveCommitIdentity({
      env: { CUENV_ENVIRONMENT: "production" },
      execSync: noGit,
    })).toThrow("production/Faro builds require Git metadata");
  });

  test("explicit immutable deploy mode rejects a dirty checkout", () => {
    expect(() => resolveCommitIdentity({
      env: { WADDLE_REQUIRE_IMMUTABLE_BUILD: "true" },
      execSync: gitExecutor({ status: " M src/app.ts\n" }),
    })).toThrow("production/Faro builds require a clean git worktree");
  });
});
