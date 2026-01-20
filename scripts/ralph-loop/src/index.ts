import { readState, writeState } from "./state.js";
import { hasUncommittedChanges, commitPhaseTransition } from "./git.js";
import { run as runPlan } from "./plan/index.js";
import { run as runBuild } from "./build/index.js";
import { run as runReview } from "./review/index.js";
import type { Config, Phase, State, PhaseResult } from "./types.js";

const phases: Record<
  Exclude<Phase, "END">,
  (state: State, config: Config) => Promise<PhaseResult>
> = {
  PLAN: runPlan,
  BUILD: runBuild,
  REVIEW: runReview,
};

function parseArgs(): Config {
  const args = process.argv.slice(2);
  const config: Config = {
    maxRetries: 3,
    maxTurns: 20,
    targetDoc: "docs/PROJECT_MANAGEMENT.md",
    dryRun: false,
  };

  for (let i = 0; i < args.length; i++) {
    const arg = args[i];
    if (arg === "--max-retries" && args[i + 1]) {
      config.maxRetries = parseInt(args[++i], 10);
    } else if (arg === "--max-turns" && args[i + 1]) {
      config.maxTurns = parseInt(args[++i], 10);
    } else if (arg === "--target-doc" && args[i + 1]) {
      config.targetDoc = args[++i];
    } else if (arg === "--dry-run") {
      config.dryRun = true;
    } else if (arg === "--phase" && args[i + 1]) {
      const phase = args[++i].toUpperCase() as Phase;
      if (["PLAN", "BUILD", "REVIEW"].includes(phase)) {
        config.startPhase = phase;
      }
    }
  }

  return config;
}

async function main() {
  const config = parseArgs();

  console.log("Ralph Loop Starting");
  console.log(`  Max Retries: ${config.maxRetries}`);
  console.log(`  Max Turns: ${config.maxTurns}`);
  console.log(`  Target Doc: ${config.targetDoc}`);
  console.log(`  Dry Run: ${config.dryRun}`);
  console.log("");

  let state = await readState();

  // Override phase if specified
  if (config.startPhase) {
    state = await writeState({ ...state, phase: config.startPhase });
  }

  let retryCount = 0;

  while (state.phase !== "END") {
    console.log(`\n${"=".repeat(60)}`);
    console.log(`PHASE: ${state.phase} (Iteration ${state.iteration})`);
    console.log(`${"=".repeat(60)}\n`);

    const runner = phases[state.phase];
    const result = await runner(state, config);

    // Track BUILD↔REVIEW retries
    if (state.phase === "REVIEW" && result.nextPhase === "BUILD") {
      retryCount++;
      console.log(`\n[Retry ${retryCount}/${config.maxRetries}]`);
      if (retryCount >= config.maxRetries) {
        console.log(`Max retries reached, forcing PLAN`);
        result.nextPhase = "PLAN";
        result.reason = `Max retries (${config.maxRetries}) reached, forcing replan`;
        retryCount = 0;
      }
    } else if (result.nextPhase === "PLAN") {
      retryCount = 0;
    }

    // Commit changes if any
    if (!config.dryRun && hasUncommittedChanges()) {
      console.log(`\nCommitting: ${state.phase} → ${result.nextPhase}`);
      commitPhaseTransition(state.phase, result.nextPhase, result.reason);
    } else if (config.dryRun) {
      console.log("\n[Dry run - skipping commit]");
    }

    // Update state
    state = await writeState({
      ...state,
      ...result.stateUpdates,
      phase: result.nextPhase,
      history: [
        ...state.history,
        {
          phase: state.phase,
          transition: result.nextPhase,
          reason: result.reason,
          timestamp: new Date().toISOString(),
        },
      ],
    });

    console.log(`\nTransition: ${result.reason}`);
  }

  console.log("\n" + "=".repeat(60));
  console.log("Ralph Loop Complete");
  console.log("=".repeat(60));
}

main().catch((error) => {
  console.error("Fatal error:", error);
  process.exit(1);
});
