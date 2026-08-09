import { existsSync, mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { pathToFileURL } from "node:url";
import { createSSRApp, h, type App } from "vue";
import { parse, compileScript } from "vue/compiler-sfc";
import { renderToString } from "vue/server-renderer";
import ts from "typescript";

/**
 * SSR render harness for `.vue` single-file components in `bun test`.
 *
 * Compiles a target SFC — and any `.vue` components it imports,
 * recursively — to ESM via `vue/compiler-sfc` + the TypeScript
 * transpiler, then renders it with `renderToString`. `@/` and relative
 * specifiers resolve against `chat/src`; bare specifiers resolve via the
 * Node resolver.
 *
 * Nested `.vue` imports are compiled into sibling ESM modules rather than
 * stubbed, so child components (e.g. a row's embedded card) render for
 * real and their markup appears in the returned HTML.
 *
 * Nanostores composables work under this harness because `@nanostores/vue`
 * reads `store.get()` synchronously when `window` is undefined (the
 * default in `bun test`), so a render reflects whatever the stores were
 * seeded with — no mount lifecycle required.
 */
export async function renderVueComponent(
  path: string,
  props: Record<string, unknown> = {},
  importMetaUrl: string,
  configureApp?: (app: App<Element>) => void,
): Promise<string> {
  const outDir = mkdtempSync(join(tmpdir(), "waddle-vue-sfc-"));
  const moduleUrl = await compileSfcModule(new URL(path, importMetaUrl), outDir, new Map());
  const component = (await import(moduleUrl)).default;
  const app = createSSRApp({ render: () => h(component, props) });
  configureApp?.(app);
  return renderToString(app);
}

export async function renderVueComponentSource(
  source: string,
  props: Record<string, unknown> = {},
  configureApp?: (app: App<Element>) => void,
): Promise<string> {
  const outDir = mkdtempSync(join(tmpdir(), "waddle-vue-sfc-"));
  const sourcePath = join(outDir, "inline.vue");
  writeFileSync(sourcePath, source);
  const moduleUrl = await compileSfcModule(pathToFileURL(sourcePath), outDir, new Map());
  const component = (await import(moduleUrl)).default;
  const app = createSSRApp({ render: () => h(component, props) });
  configureApp?.(app);
  return renderToString(app);
}

async function loadVueComponent(
  path: string,
  importMetaUrl: string,
  options: { inlineTemplate?: boolean } = {},
): Promise<unknown> {
  const outDir = mkdtempSync(join(tmpdir(), "waddle-vue-sfc-"));
  const moduleUrl = await compileSfcModule(
    new URL(path, importMetaUrl),
    outDir,
    new Map(),
    options,
  );
  return (await import(moduleUrl)).default;
}

export async function setupVueComponent(
  path: string,
  props: Record<string, unknown>,
  importMetaUrl: string,
  emit: (...args: unknown[]) => void = () => undefined,
): Promise<Record<string, unknown>> {
  const component = await loadVueComponent(path, importMetaUrl, { inlineTemplate: false }) as {
    setup?: (
      props: Record<string, unknown>,
      context: {
        emit: (...args: unknown[]) => void;
        expose: () => void;
        attrs: Record<string, unknown>;
        slots: Record<string, unknown>;
      },
    ) => Record<string, unknown>;
  };
  if (!component.setup) throw new Error(`${path} has no setup function`);
  return component.setup(props, {
    emit,
    expose: () => undefined,
    attrs: {},
    slots: {},
  });
}

/**
 * Compiles `filename` (a `.vue` SFC) into an ESM module written under
 * `outDir`, returning its `file://` URL. `cache` dedupes shared child
 * SFCs across the dependency graph so each is compiled once.
 */
async function compileSfcModule(
  filename: URL,
  outDir: string,
  cache: Map<string, string>,
  options: { inlineTemplate?: boolean } = {},
): Promise<string> {
  const cached = cache.get(filename.pathname);
  if (cached) return cached;

  const source = readFileSync(filename, "utf8");
  const { descriptor } = parse(source, { filename: filename.pathname });
  const script = compileScript(descriptor, {
    id: filename.pathname,
    inlineTemplate: options.inlineTemplate ?? true,
  });

  const transpiled = ts.transpileModule(
    [
      script.content.replace("export default", "const __sfc__ ="),
      "export default __sfc__;",
    ].join("\n"),
    {
      compilerOptions: {
        module: ts.ModuleKind.ESNext,
        target: ts.ScriptTarget.ES2022,
        verbatimModuleSyntax: false,
      },
    },
  ).outputText;

  const rewritten = await rewriteImports(transpiled, filename, outDir, cache, options);
  const modulePath = join(outDir, `sfc-${cache.size}.mjs`);
  const moduleUrl = pathToFileURL(modulePath).href;
  // Reserve the cache slot before writing so recursive imports see it.
  cache.set(filename.pathname, moduleUrl);
  writeFileSync(modulePath, rewritten);
  return moduleUrl;
}

async function rewriteImports(
  code: string,
  importer: URL,
  outDir: string,
  cache: Map<string, string>,
  options: { inlineTemplate?: boolean } = {},
): Promise<string> {
  const specifiers = [...code.matchAll(/from\s+["']([^"']+)["']/g)].map((m) => m[1]);
  let result = code;
  for (const specifier of specifiers) {
    const resolved = await resolveModuleSpecifier(specifier, importer, outDir, cache, options);
    result = result.replaceAll(`from "${specifier}"`, `from "${resolved}"`);
    result = result.replaceAll(`from '${specifier}'`, `from "${resolved}"`);
  }
  return result;
}

async function resolveModuleSpecifier(
  specifier: string,
  importer: URL,
  outDir: string,
  cache: Map<string, string>,
  options: { inlineTemplate?: boolean } = {},
): Promise<string> {
  if (specifier.startsWith("@/")) {
    const path = resolveSourcePath(new URL(`../../src/${specifier.slice(2)}`, import.meta.url).pathname);
    return moduleUrlForPath(path, outDir, cache, options);
  }
  if (specifier.startsWith(".")) {
    const path = resolveSourcePath(new URL(specifier, importer).pathname);
    return moduleUrlForPath(path, outDir, cache, options);
  }
  return import.meta.resolve(specifier);
}

async function moduleUrlForPath(
  resolvedPath: string,
  outDir: string,
  cache: Map<string, string>,
  options: { inlineTemplate?: boolean } = {},
): Promise<string> {
  if (resolvedPath.endsWith(".vue")) {
    return compileSfcModule(pathToFileURL(resolvedPath), outDir, cache, options);
  }
  return pathToFileURL(resolvedPath).href;
}

function resolveSourcePath(basePath: string): string {
  const candidates = [
    basePath,
    `${basePath}.ts`,
    `${basePath}.tsx`,
    `${basePath}.js`,
    `${basePath}.mjs`,
    `${basePath}.vue`,
    `${basePath}.json`,
    `${basePath}/index.ts`,
  ];
  const resolved = candidates.find((candidate) => existsSync(candidate));
  if (!resolved) throw new Error(`Unable to resolve test SFC import: ${basePath}`);
  return resolved;
}
