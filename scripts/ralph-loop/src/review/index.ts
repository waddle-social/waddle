import { query } from "@anthropic-ai/claude-agent-sdk";
import { getGitRoot, getDiffFromLastPhase } from "../git.js";
import type { State, Config, PhaseResult, Phase } from "../types.js";

function buildPrompt(state: State, targetDoc: string, diff: string): string {
  const plan = state.plan;

  return `# Ralph Loop - REVIEW Phase (Iteration ${state.iteration})

You are reviewing the implementation against the plan.

## The Plan
${plan ? `**Task:** ${plan.task}

**Acceptance Criteria:**
${plan.acceptanceCriteria.map((c) => `- ${c}`).join("\n")}` : "No plan available - reviewing existing changes."}

## Changes Made
\`\`\`diff
${diff || "No changes detected."}
\`\`\`

## Your Task

1. Review the diff against the acceptance criteria
2. Check that ${targetDoc} is updated appropriately
3. Run tests/builds to verify everything works
4. Identify any issues or improvements needed

## Output Format

Provide your review assessment:
<review>
<status>PASS|FAIL|PARTIAL</status>
<feedback>Your detailed feedback</feedback>
<issues>
- Issue 1 (if any)
- Issue 2 (if any)
</issues>
</review>

Then output your transition:
<transition to="PLAN|BUILD|END|REVIEW" reason="Your reasoning">

- Use END if the task is complete and meets all criteria
- Use BUILD if there are fixes needed
- Use PLAN if the approach needs to change or next iteration should start
- Use REVIEW if you need more review cycles
`;
}

function parseReview(content: string): { feedback: string; issues: string[] } {
  const reviewMatch = content.match(/<review>([\s\S]*?)<\/review>/);
  if (!reviewMatch) {
    return { feedback: "", issues: [] };
  }

  const reviewContent = reviewMatch[1];
  const feedbackMatch = reviewContent.match(/<feedback>([\s\S]*?)<\/feedback>/);
  const issuesMatch = reviewContent.match(/<issues>([\s\S]*?)<\/issues>/);

  const issues = issuesMatch
    ? issuesMatch[1]
        .split("\n")
        .map((line) => line.replace(/^[\s-]*/, "").trim())
        .filter((line) => line.length > 0)
    : [];

  return {
    feedback: feedbackMatch?.[1]?.trim() || "",
    issues,
  };
}

function parseTransition(content: string): { nextPhase: Phase; reason: string } {
  const match = content.match(/<transition to="(PLAN|BUILD|END|REVIEW)" reason="([^"]*)">/);
  if (match) {
    return { nextPhase: match[1] as Phase, reason: match[2] };
  }
  return { nextPhase: "PLAN", reason: "No explicit transition found, starting next iteration" };
}

export async function run(state: State, config: Config): Promise<PhaseResult> {
  const diff = getDiffFromLastPhase();
  const prompt = buildPrompt(state, config.targetDoc, diff);

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
          console.log(`\n[REVIEW Session: ${message.session_id}]`);
        }
        break;
      case "result":
        if (message.subtype === "success") {
          console.log("\n[REVIEW Complete]");
        } else {
          console.error("\n[REVIEW Error]:", message.errors?.join(", ") ?? "Unknown error");
        }
        break;
    }
  }

  const { feedback, issues } = parseReview(lastContent);
  const transition = parseTransition(lastContent);

  const stateUpdates: Partial<State> = {
    review: { lastFeedback: feedback, issues },
  };

  // If transitioning to PLAN, increment iteration
  if (transition.nextPhase === "PLAN") {
    stateUpdates.iteration = state.iteration + 1;
  }

  return {
    nextPhase: transition.nextPhase,
    reason: transition.reason,
    stateUpdates,
  };
}
