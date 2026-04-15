import { bundledLanguages, codeToHtml, type BundledLanguage } from "shiki";

const cache = new Map<string, string>();

const themes = {
  light: "github-light",
  dark: "github-dark",
} as const;

const languageAliases: Record<string, string> = {
  js: "javascript",
  ts: "typescript",
  py: "python",
  rs: "rust",
  sh: "bash",
  shell: "bash",
  zsh: "bash",
  text: "markdown",
  txt: "markdown",
};

const supportedLanguages = new Set(
  Object.keys(bundledLanguages) as BundledLanguage[],
);

function normalizeLanguage(raw?: string): BundledLanguage {
  const source = (raw ?? "").trim().toLowerCase();
  if (!source) return "markdown";
  const firstToken = source.split(/[\s,{]/)[0] ?? source;
  const aliased = languageAliases[firstToken] ?? firstToken;
  return supportedLanguages.has(aliased as BundledLanguage)
    ? aliased as BundledLanguage
    : "markdown";
}

async function highlight(code: string, language?: string): Promise<string> {
  const lang = normalizeLanguage(language);
  const key = `${lang}\u0000${code}`;
  const cached = cache.get(key);
  if (cached) return cached;

  try {
    const html = await codeToHtml(code, { lang, themes });
    cache.set(key, html);
    return html;
  } catch {
    const html = await codeToHtml(code, { lang: "markdown", themes });
    cache.set(key, html);
    return html;
  }
}

/**
 * Replace markdown-rendered code blocks under `root` with Shiki-highlighted HTML.
 * This runs on the client after v-html content has been injected.
 */
export async function applyShikiToCodeBlocks(root: HTMLElement): Promise<void> {
  const blocks = root.querySelectorAll<HTMLPreElement>('pre[data-code-block="true"]');
  for (const block of blocks) {
    if (block.dataset.shikiReady === "true") continue;
    block.dataset.shikiReady = "true";

    const codeEl = block.querySelector("code");
    if (!codeEl) continue;

    const code = codeEl.textContent ?? "";
    const language = block.dataset.language;
    const html = await highlight(code, language);

    const template = document.createElement("template");
    template.innerHTML = html.trim();
    const replacement = template.content.firstElementChild;
    if (!(replacement instanceof HTMLPreElement)) continue;

    replacement.classList.add("message-code-block");
    replacement.setAttribute("data-shiki", "true");
    block.replaceWith(replacement);
  }
}
