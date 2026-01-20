import { execSync } from "child_process";

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

export function getLastRalphCommit(): string | null {
  try {
    const result = execSync(
      `git log --oneline --grep="ralph-loop: iteration" -1 --format="%H"`,
      execOpts
    ).trim();
    return result || null;
  } catch {
    return null;
  }
}

export function getDiffFromLastRalph(): string {
  const lastCommit = getLastRalphCommit();
  if (lastCommit) {
    return getDiff(lastCommit);
  }
  return "";
}

export function getLastIterationNumber(): number {
  try {
    const result = execSync(
      `git log --oneline --grep="ralph-loop: iteration" -1 --format="%s"`,
      execOpts
    ).trim();
    const match = result.match(/ralph-loop: iteration (\d+)/);
    return match ? parseInt(match[1], 10) : 0;
  } catch {
    return 0;
  }
}

export function commitChanges(iteration: number, summary: string): void {
  execSync(`git add -A`, execOpts);
  const message = `ralph-loop: iteration ${iteration}\n\n${summary}`;
  execSync(`git commit -m "${message.replace(/"/g, '\\"')}"`, execOpts);
}

export function hasUncommittedChanges(): boolean {
  try {
    const status = execSync(`git status --porcelain`, execOpts);
    return status.trim().length > 0;
  } catch {
    return false;
  }
}
