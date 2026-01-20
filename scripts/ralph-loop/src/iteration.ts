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
        cwd: getGitRoot(),
        permissionMode: "bypassPermissions",
        maxTurns: 20,
        includePartialMessages: true,
        stderr: (data: string) => {
          process.stderr.write(data);
        },
      },
    });

    for await (const message of response) {
      switch (message.type) {
        case "assistant":
          // assistant messages have a BetaMessage in message.message
          if (Array.isArray(message.message.content)) {
            for (const block of message.message.content) {
              if (block.type === "text") {
                lastContent = block.text;
              }
            }
          }
          break;
        case "stream_event":
          // Partial streaming events from includePartialMessages
          if (
            message.event.type === "content_block_delta" &&
            message.event.delta.type === "text_delta"
          ) {
            process.stdout.write(message.event.delta.text);
          }
          break;
        case "tool_progress":
          console.log(`\n[Tool: ${message.tool_name}]`);
          break;
        case "system":
          if (message.subtype === "init") {
            console.log(`[Session: ${message.session_id}]`);
          }
          break;
        case "result":
          if (message.subtype === "success") {
            console.log("\n[Session Complete]");
          } else {
            console.error("\n[Error]:", message.errors?.join(", ") ?? "Unknown error");
          }
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
