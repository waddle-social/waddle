import { execSync } from "node:child_process";
const FULL_COMMIT_SHA = /^[0-9a-f]{40}$/i;
const SUPPLIED_SHA_NAMES = ["WADDLE_GIT_SHA", "GITHUB_SHA", "CF_PAGES_COMMIT_SHA"];

export function resolveCommitSha(options = {}) {
  return resolveCommitIdentity(options).commitSha;
}

export function resolveCommitIdentity(options = {}) {
  const env = options.env ?? process.env;
  const execute = options.execSync ?? execSync;
  const cwd = options.cwd;
  const immutableBuild = options.requireImmutable ?? isImmutableBuild(env);
  const supplied = suppliedCommitShas(env);
  const gitHead = resolveGitHead(execute, cwd);

  if (gitHead) {
    for (const [name, sha] of supplied) {
      if (sha !== gitHead) {
        throw new Error(`${name} does not match git HEAD ${gitHead}`);
      }
    }
    if (immutableBuild) assertCleanGitWorktree(execute, cwd);
    return {
      commitSha: gitHead,
      source: { kind: "git", commitSha: gitHead },
    };
  }

  if (supplied.length > 0) {
    throw new Error(
      "a supplied commit SHA cannot attest a source archive; immutable builds require Git metadata",
    );
  }
  if (immutableBuild) {
    throw new Error("production/Faro builds require Git metadata");
  }
  return {
    commitSha: "unknown",
    source: { kind: "unknown", commitSha: "unknown" },
  };
}

export function isImmutableBuild(env = process.env) {
  return env.WADDLE_REQUIRE_IMMUTABLE_BUILD === "true"
    || env.CUENV_ENVIRONMENT === "production"
    || (typeof env.PUBLIC_FARO_URL === "string" && env.PUBLIC_FARO_URL.trim().length > 0);
}

function suppliedCommitShas(env) {
  return SUPPLIED_SHA_NAMES.flatMap((name) => {
    const value = env[name];
    if (typeof value !== "string" || value.trim().length === 0) return [];
    const sha = exactCommitSha(value);
    if (!sha) throw new Error(`${name} must be a full 40-character commit SHA`);
    return [[name, sha]];
  });
}

function resolveGitHead(execute, cwd) {
  try {
    const inside = runGit(execute, "git rev-parse --is-inside-work-tree", cwd);
    if (inside !== "true") return null;
  } catch {
    return null;
  }
  const head = exactCommitSha(runGit(execute, "git rev-parse HEAD", cwd));
  if (!head) throw new Error("git HEAD is not a full 40-character commit SHA");
  return head;
}

function assertCleanGitWorktree(execute, cwd) {
  const status = runGit(execute, "git status --porcelain --untracked-files=normal", cwd);
  if (status.length > 0) {
    throw new Error("production/Faro builds require a clean git worktree");
  }
}

function runGit(execute, command, cwd) {
  return execute(command, {
    stdio: ["ignore", "pipe", "ignore"],
    ...(cwd ? { cwd } : {}),
  })
    .toString()
    .trim();
}

function exactCommitSha(value) {
  if (typeof value !== "string") return null;
  const trimmed = value.trim();
  return FULL_COMMIT_SHA.test(trimmed) ? trimmed.toLowerCase() : null;
}
