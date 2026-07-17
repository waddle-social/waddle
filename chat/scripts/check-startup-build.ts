import { readdirSync, readFileSync } from "node:fs";

export const STARTUP_SHELL_MARKERS = [
  "chat-app-shell chat-startup-shell",
  "Loading Waddle",
  "Checking session.",
] as const;

type StartupChunk = {
  name: string;
  contents: string;
};

function listed(names: readonly string[]): string {
  return names.length > 0 ? names.join(", ") : "(none)";
}

/**
 * Select the one emitted chunk that owns the startup shell.
 *
 * Rollup filenames are unstable, but the shell marker must have exactly one
 * owner and that owner must contain the complete fallback contract.
 */
export function selectStartupShellChunk(
  chunks: readonly StartupChunk[],
): StartupChunk {
  const ordered = [...chunks].sort((left, right) =>
    left.name.localeCompare(right.name)
  );
  const candidates = ordered.filter((chunk) =>
    chunk.contents.includes(STARTUP_SHELL_MARKERS[0])
  );

  if (candidates.length === 0) {
    throw new Error(
      `no startup-shell candidate found; scanned chunks: ${listed(ordered.map((chunk) => chunk.name))}`,
    );
  }
  if (candidates.length > 1) {
    throw new Error(
      `ambiguous startup-shell candidates: ${listed(candidates.map((chunk) => chunk.name))}`,
    );
  }

  const candidate = candidates[0]!;
  const missing = STARTUP_SHELL_MARKERS.filter(
    (marker) => !candidate.contents.includes(marker),
  );
  if (missing.length > 0) {
    throw new Error(
      `startup-shell candidate ${candidate.name} is missing markers: ${missing.map((marker) => JSON.stringify(marker)).join(", ")}`,
    );
  }
  return candidate;
}

if (import.meta.main) {
  const chunksDir = new URL("../dist/server/chunks/", import.meta.url);
  const chunkNames = readdirSync(chunksDir)
    .filter((name) => name.endsWith(".mjs"))
    .sort((left, right) => left.localeCompare(right));
  const candidate = selectStartupShellChunk(
    chunkNames.map((name) => ({
      name,
      contents: readFileSync(new URL(name, chunksDir), "utf8"),
    })),
  );
  console.log(
    `startup fallback is present in canonical chat build chunk ${candidate.name}`,
  );
}
