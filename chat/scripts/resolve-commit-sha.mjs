import { execSync } from "node:child_process";

export function resolveCommitSha() {
  const envSha = process.env.WADDLE_GIT_SHA ?? process.env.CF_PAGES_COMMIT_SHA;
  if (envSha && envSha.trim().length > 0) return envSha.trim().slice(0, 12);
  try {
    return execSync("git rev-parse --short=12 HEAD", { stdio: ["ignore", "pipe", "ignore"] })
      .toString()
      .trim();
  } catch {
    return "unknown";
  }
}
