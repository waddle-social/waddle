import { query } from "@anthropic-ai/claude-agent-sdk";
import { buildIterationPrompt } from "./prompts.js";
import { getGitRoot } from "./git.js";
import type { IterationResult } from "./types.js";

export async function runIteration(
  n: number,
  diff: string,
  targetDoc: string
): Promise<IterationResult> {
  const startTime = Date.now();
  const prompt = buildIterationPrompt(n, diff, targetDoc);

  let commitSummary: string | null = null;
  let lastContent = "";

  try {
    const response = query({
      prompt,
      options: {
        model: "claude-sonnet-4-5",
        workingDirectory: getGitRoot(),
        permissionMode: "bypassPermissions",
        maxTurns: 20,
      },
    });

    for await (const message of response) {
      switch (message.type) {
        case "assistant":
          if (typeof message.content === "string") {
            lastContent = message.content;
            process.stdout.write(message.content);
          } else if (Array.isArray(message.content)) {
            for (const block of message.content) {
              if (block.type === "text") {
                lastContent = block.text;
                process.stdout.write(block.text);
              }
            }
          }
          break;
        case "tool_call":
          console.log(`\n[Tool: ${message.tool_name}]`);
          break;
        case "error":
          console.error("\n[Error]:", message.error);
          break;
      }
    }

    // Extract commit summary from the response
    const summaryMatch = lastContent.match(
      /<commit-summary>([\s\S]*?)<\/commit-summary>/
    );
    if (summaryMatch) {
      commitSummary = summaryMatch[1].trim();
    }

    return {
      iteration: n,
      success: true,
      commitSummary,
      duration: Date.now() - startTime,
    };
  } catch (error) {
    return {
      iteration: n,
      success: false,
      commitSummary: null,
      error: error instanceof Error ? error.message : String(error),
      duration: Date.now() - startTime,
    };
  }
}
