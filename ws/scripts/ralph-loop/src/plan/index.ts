import { query } from "@anthropic-ai/claude-agent-sdk";
import { getGitRoot, getRecentCommits } from "../git.js";
import type { State, Config, PhaseResult, Plan } from "../types.js";

function buildPrompt(state: State, targetDoc: string, recentCommits: string): string {
  return `# Ralph Loop - PLAN Phase (Iteration ${state.iteration})

You are analyzing the project to identify and plan the next task.

## Target Document
Read and understand: ${targetDoc}

## Recent Git History
${recentCommits || "No recent commits."}

## Previous Context
${state.review.lastFeedback ? `Last review feedback: ${state.review.lastFeedback}` : "Fresh start - no previous feedback."}
${state.build.blockers.length > 0 ? `Previous blockers: ${state.build.blockers.join(", ")}` : ""}

## Your Task

1. Read ${targetDoc} to understand the project state and priorities
2. Review recent git history to understand what's been done
3. Identify the single most important next task
4. Create a structured plan

## Output Format

You MUST output a plan in this exact format:
<plan>
<task>One-line description of what to implement</task>
<files>
- path/to/file1.rs
- path/to/file2.rs
</files>
<steps>
- Step 1: What to do first
- Step 2: What to do next
</steps>
<acceptance-criteria>
- Criterion 1: How to verify success
- Criterion 2: Another verification
</acceptance-criteria>
</plan>

Then output your transition decision:
<transition to="BUILD|END" reason="Your reasoning">

Use BUILD if there's work to do.
Use END if the project is complete or there's nothing actionable.
`;
}

function parsePlan(content: string): Plan | null {
  const planMatch = content.match(/<plan>([\s\S]*?)<\/plan>/);
  if (!planMatch) return null;

  const planContent = planMatch[1];

  const taskMatch = planContent.match(/<task>([\s\S]*?)<\/task>/);
  const filesMatch = planContent.match(/<files>([\s\S]*?)<\/files>/);
  const stepsMatch = planContent.match(/<steps>([\s\S]*?)<\/steps>/);
  const criteriaMatch = planContent.match(/<acceptance-criteria>([\s\S]*?)<\/acceptance-criteria>/);

  const parseList = (text: string | undefined): string[] => {
    if (!text) return [];
    return text
      .split("\n")
      .map((line) => line.replace(/^[\s-]*/, "").trim())
      .filter((line) => line.length > 0);
  };

  return {
    task: taskMatch?.[1]?.trim() || "Unknown task",
    files: parseList(filesMatch?.[1]),
    steps: parseList(stepsMatch?.[1]),
    acceptanceCriteria: parseList(criteriaMatch?.[1]),
  };
}

function parseTransition(content: string): { nextPhase: "BUILD" | "END"; reason: string } {
  const match = content.match(/<transition to="(BUILD|END)" reason="([^"]*)">/);
  if (match) {
    return { nextPhase: match[1] as "BUILD" | "END", reason: match[2] };
  }
  return { nextPhase: "BUILD", reason: "No explicit transition found, defaulting to BUILD" };
}

export async function run(state: State, config: Config): Promise<PhaseResult> {
  const recentCommits = getRecentCommits(10);
  const prompt = buildPrompt(state, config.targetDoc, recentCommits);

  let lastContent = "";

  const response = query({
    prompt,
    options: {
      model: "claude-opus-4-5",
      cwd: getGitRoot(),
      permissionMode: "bypassPermissions",
      maxTurns: config.maxTurns,
      includePartialMessages: true,
      stderr: (data: string) => process.stderr.write(data),
    },
  });

  for await (const message of response) {
    switch (message.type) {
      case "assistant":
        if (Array.isArray(message.message.content)) {
          for (const block of message.message.content) {
            if (block.type === "text") {
              lastContent = block.text;
            }
          }
        }
        break;
      case "stream_event":
        if (
          message.event.type === "content_block_delta" &&
          message.event.delta.type === "text_delta"
        ) {
          process.stdout.write(message.event.delta.text);
        }
        break;
      case "system":
        if (message.subtype === "init") {
          console.log(`\n[PLAN Session: ${message.session_id}]`);
        }
        break;
      case "result":
        if (message.subtype === "success") {
          console.log("\n[PLAN Complete]");
        } else {
          console.error("\n[PLAN Error]:", message.errors?.join(", ") ?? "Unknown error");
        }
        break;
    }
  }

  const plan = parsePlan(lastContent);
  const transition = parseTransition(lastContent);

  return {
    nextPhase: transition.nextPhase,
    reason: transition.reason,
    stateUpdates: {
      plan,
      build: { stepsCompleted: [], blockers: [] },
      review: { lastFeedback: null, issues: [] },
    },
  };
}
