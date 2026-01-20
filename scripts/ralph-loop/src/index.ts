import { writeFileSync, mkdirSync, existsSync } from "fs";
import { join, dirname } from "path";
import { fileURLToPath } from "url";
import { runIteration } from "./iteration.js";
import {
  getDiffFromLastRalph,
  getLastIterationNumber,
  commitChanges,
  hasUncommittedChanges,
} from "./git.js";
import type { Config, IterationLog } from "./types.js";

const __dirname = dirname(fileURLToPath(import.meta.url));
const logsDir = join(__dirname, "..", "logs");

function parseArgs(): Config {
  const args = process.argv.slice(2);
  const config: Config = {
    iterations: 60,
    targetDoc: "docs/PROJECT_MANAGEMENT.md",
    dryRun: false,
  };

  for (let i = 0; i < args.length; i++) {
    const arg = args[i];
    if (arg === "--iterations" && args[i + 1]) {
      config.iterations = parseInt(args[++i], 10);
    } else if (arg === "--target-doc" && args[i + 1]) {
      config.targetDoc = args[++i];
    } else if (arg === "--dry-run") {
      config.dryRun = true;
    }
  }

  return config;
}

function saveLog(log: IterationLog): void {
  if (!existsSync(logsDir)) {
    mkdirSync(logsDir, { recursive: true });
  }
  const filename = `iteration-${log.iteration}.json`;
  writeFileSync(join(logsDir, filename), JSON.stringify(log, null, 2));
}

async function main() {
  const config = parseArgs();
  console.log("Ralph Loop Starting");
  console.log(`  Iterations: ${config.iterations}`);
  console.log(`  Target Doc: ${config.targetDoc}`);
  console.log(`  Dry Run: ${config.dryRun}`);
  console.log("");

  const startIteration = getLastIterationNumber() + 1;

  for (let i = 0; i < config.iterations; i++) {
    const iterationNumber = startIteration + i;
    console.log(`\n${"=".repeat(60)}`);
    console.log(`ITERATION ${iterationNumber}`);
    console.log(`${"=".repeat(60)}\n`);

    const diff = getDiffFromLastRalph();
    const result = await runIteration(iterationNumber, diff, config.targetDoc);

    const log: IterationLog = {
      iteration: iterationNumber,
      timestamp: new Date().toISOString(),
      config,
      diff,
      result,
    };

    saveLog(log);

    if (!result.success) {
      console.error(`\nIteration ${iterationNumber} failed:`, result.error);
      process.exit(1);
    }

    if (!config.dryRun && hasUncommittedChanges()) {
      const summary = result.commitSummary || `Iteration ${iterationNumber} completed`;
      console.log(`\nCommitting changes: ${summary}`);
      commitChanges(iterationNumber, summary);
    } else if (config.dryRun) {
      console.log("\n[Dry run - skipping commit]");
    } else {
      console.log("\n[No changes to commit]");
    }

    console.log(`\nIteration ${iterationNumber} completed in ${result.duration}ms`);
  }

  console.log("\n" + "=".repeat(60));
  console.log("Ralph Loop Complete");
  console.log("=".repeat(60));
}

main().catch((error) => {
  console.error("Fatal error:", error);
  process.exit(1);
});
