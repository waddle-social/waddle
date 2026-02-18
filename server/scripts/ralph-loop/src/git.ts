import { execSync } from "child_process";
import type { Phase } from "./types.js";

export function getGitRoot(): string {
  return execSync("git rev-parse --show-toplevel", { encoding: "utf-8" }).trim();
}

const gitRoot = getGitRoot();
const execOpts = { encoding: "utf-8" as const, cwd: gitRoot };

export function getDiff(baseRef = "HEAD~1"): string {
  try {
    return execSync(`git diff ${baseRef}`, execOpts);
  } catch {
    return "";
  }
}

export function getRecentCommits(count = 10): string {
  try {
    return execSync(
      `git log --oneline -${count} --format="%h %s"`,
      execOpts
    ).trim();
  } catch {
    return "";
  }
}

export function getLastRalphCommit(): string | null {
  try {
    const result = execSync(
      `git log --oneline --grep="^ralph:" -1 --format="%H"`,
      execOpts
    ).trim();
    return result || null;
  } catch {
    return null;
  }
}

export function getDiffFromLastPhase(): string {
  const lastCommit = getLastRalphCommit();
  if (lastCommit) {
    return getDiff(lastCommit);
  }
  return getDiff("HEAD~1");
}

export function hasUncommittedChanges(): boolean {
  try {
    const status = execSync(`git status --porcelain`, execOpts);
    return status.trim().length > 0;
  } catch {
    return false;
  }
}

export function commitPhaseTransition(
  fromPhase: Phase,
  toPhase: Phase,
  reason: string
): void {
  execSync(`git add -A`, execOpts);

  const message = `ralph: ${fromPhase} → ${toPhase}\n\n${reason}`;
  const escapedMessage = message.replace(/"/g, '\\"');
  execSync(`git commit -m "${escapedMessage}"`, execOpts);
}

export function getCurrentBranch(): string {
  try {
    return execSync("git branch --show-current", execOpts).trim();
  } catch {
    return "unknown";
  }
}
