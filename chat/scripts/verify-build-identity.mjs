import { lstatSync, readFileSync, readdirSync } from "node:fs";
import { dirname, extname, relative, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";
import { parse as parseJavaScriptModule } from "acorn";
import {
  assertBuildIdentityMatches,
  BUILD_IDENTITY_FILE_NAME,
  parseBuildIdentity,
  resolveBuildIdentity,
  serializeBuildIdentity,
} from "./build-identity.mjs";

const scriptDir = dirname(fileURLToPath(import.meta.url));
const CLIENT_ENTRY_PREFIX = "_astro/";
const APP_LAYOUT_ENTRY_SUFFIX = "src/layouts/AppLayout.astro?astro&type=script&index=0&lang.ts";
const BUILD_MARKER_PREFIX = "waddle-build-identity-v1:";

export function verifyBuildArtifacts(options = {}) {
  const projectRoot = options.projectRoot ?? resolve(scriptDir, "..");
  const clientDir = options.clientDir ?? resolve(projectRoot, "dist", "client");
  const serverDir = options.serverDir ?? resolve(projectRoot, "dist", "server");
  const expected = options.expectedIdentity ?? resolveBuildIdentity(options);
  // Anchor both artifact trees to a real repository path before trusting
  // per-file lstat checks. Otherwise a symlinked `dist` ancestor makes every
  // descendant look regular while redirecting verification outside the build.
  assertDirectory(clientDir, projectRoot);
  assertDirectory(serverDir, projectRoot);
  const artifactPath = resolve(clientDir, BUILD_IDENTITY_FILE_NAME);
  assertRegularFile(artifactPath, clientDir);
  const artifactSource = readFileSync(artifactPath, "utf8");
  if (artifactSource !== serializeBuildIdentity(expected)) {
    throw new Error(`${artifactPath} is not the canonical expected build identity`);
  }
  const actual = parseBuildIdentity(artifactSource, artifactPath);
  assertBuildIdentityMatches(actual, expected, artifactPath);

  const serviceWorkerPath = resolve(clientDir, "sw.js");
  assertRegularFile(serviceWorkerPath, clientDir);
  const serviceWorker = readFileSync(serviceWorkerPath, "utf8");
  const serviceWorkerProgram = parseJavaScript(serviceWorker, serviceWorkerPath);
  const cacheBindings = topLevelVariableBindings(serviceWorkerProgram, "CACHE_NAME");
  const expectedCacheName = `waddle-${expected.commitSha}`;
  if (
    cacheBindings.length !== 1
    || cacheBindings[0].kind !== "const"
    || cacheBindings[0].declaration.init?.type !== "Literal"
    || cacheBindings[0].declaration.init.value !== expectedCacheName
    || serviceWorker.includes("__WADDLE_BUILD_SHA__")
  ) {
    throw new Error(`service worker must contain exactly one top-level immutable cache identity ${expectedCacheName}`);
  }

  const { manifest, workerPath, workerProgram } = readAstroManifest(serverDir);
  assertOnlyExpectedMarkerLiterals(workerProgram, expected.marker, workerPath, true);
  const { wranglerConfigPath, entryPath } = readDeploymentEntrypoint(serverDir, workerPath);

  const manifestAssets = manifestClientJavaScriptAssets(manifest);
  const bundleFiles = javascriptFiles(resolve(clientDir, "_astro"), clientDir);
  if (bundleFiles.length === 0) throw new Error("client build contains no JavaScript bundles");
  const bundleAssets = new Set(bundleFiles.map((path) => clientAssetPath(clientDir, path)));
  assertEqualSets(bundleAssets, manifestAssets, "client JavaScript assets do not match the server manifest");

  const entryModules = manifestClientEntryModules(manifest, manifestAssets, clientDir);
  const appLayoutEntries = entryModules.filter(({ source }) => source.endsWith(APP_LAYOUT_ENTRY_SUFFIX));
  if (appLayoutEntries.length !== 1) {
    throw new Error("server manifest must contain exactly one AppLayout client entry");
  }
  const markerFile = appLayoutEntries[0].path;
  let markerFileMarkers = [];
  for (const bundlePath of bundleFiles) {
    const source = readFileSync(bundlePath, "utf8");
    const program = parseJavaScript(source, bundlePath);
    const markers = assertOnlyExpectedMarkerLiterals(program, expected.marker, bundlePath, false);
    if (bundlePath === markerFile) markerFileMarkers = markers;
  }
  if (markerFileMarkers.length !== 1 || markerFileMarkers[0] !== expected.marker) {
    throw new Error("AppLayout client entry must contain exactly one expected build identity marker literal");
  }

  if (expected.faro.enabled && expected.faro.release !== expected.commitSha) {
    throw new Error("Faro release does not match the expected full commit");
  }
  return {
    artifactPath,
    serviceWorkerPath,
    markerFile,
    workerPath,
    wranglerConfigPath,
    entryPath,
    entryModules: entryModules.map(({ path }) => path),
    identity: actual,
  };
}

function readDeploymentEntrypoint(serverDir, workerPath) {
  const wranglerConfigPath = resolve(serverDir, "wrangler.json");
  assertRegularFile(wranglerConfigPath, serverDir);
  let config;
  try {
    config = JSON.parse(readFileSync(wranglerConfigPath, "utf8"));
  } catch {
    throw new Error("generated Wrangler deployment config is not valid JSON");
  }
  if (!config || typeof config !== "object" || Array.isArray(config)) {
    throw new Error("generated Wrangler deployment config must be an object");
  }
  if (config.main !== "entry.mjs") {
    throw new Error("generated Wrangler deployment config must set main to entry.mjs");
  }
  if (config.no_bundle !== true) {
    throw new Error("generated Wrangler deployment config must enable no_bundle");
  }
  if (
    !config.assets
    || typeof config.assets !== "object"
    || Array.isArray(config.assets)
    || config.assets.binding !== "ASSETS"
    || config.assets.directory !== "../client"
  ) {
    throw new Error("generated Wrangler deployment config must bind ASSETS to ../client");
  }

  const entryPath = resolve(serverDir, config.main);
  assertRegularFile(entryPath, serverDir);
  const entryProgram = parseJavaScript(readFileSync(entryPath, "utf8"), entryPath);
  const expectedWorkerImport = `./${relative(serverDir, workerPath).split(sep).join("/")}`;
  if (entryProgram.body.length !== 5) {
    throw new Error("generated Wrangler entry must contain exactly the generated deployment wrapper statements");
  }
  const [processShim, processEnvironmentShim, cloudflareImport, workerImport, deploymentExport] = entryProgram.body;
  if (
    !isNullishObjectInitialization(processShim, ["globalThis", "process"])
    || !isNullishObjectInitialization(processEnvironmentShim, ["globalThis", "process", "env"])
    || cloudflareImport.type !== "ImportDeclaration"
    || cloudflareImport.specifiers.length !== 0
    || cloudflareImport.source?.type !== "Literal"
    || cloudflareImport.source.value !== "cloudflare:workers"
  ) {
    throw new Error("generated Wrangler entry must contain only the expected process shims and Cloudflare import");
  }
  if (
    workerImport.type !== "ImportDeclaration"
    || workerImport.source?.type !== "Literal"
    || workerImport.source.value !== expectedWorkerImport
    || workerImport.specifiers.length !== 1
    || workerImport.specifiers[0].type !== "ImportSpecifier"
  ) {
    throw new Error("generated Wrangler entry must import exactly the verified worker entry");
  }

  const workerBinding = workerImport.specifiers[0].local?.name;
  if (
    !workerBinding
    || deploymentExport.type !== "ExportNamedDeclaration"
    || deploymentExport.declaration !== null
    || deploymentExport.source !== null
    || deploymentExport.specifiers.length !== 1
    || deploymentExport.specifiers[0].type !== "ExportSpecifier"
    || deploymentExport.specifiers[0].local?.type !== "Identifier"
    || deploymentExport.specifiers[0].local.name !== workerBinding
    || deploymentExport.specifiers[0].exported?.type !== "Identifier"
    || deploymentExport.specifiers[0].exported.name !== "default"
  ) {
    throw new Error("generated Wrangler entry must export only the verified worker as default");
  }

  return { wranglerConfigPath, entryPath };
}

function isNullishObjectInitialization(statement, expectedPath) {
  if (
    statement?.type !== "ExpressionStatement"
    || statement.expression?.type !== "AssignmentExpression"
    || statement.expression.operator !== "??="
    || statement.expression.right?.type !== "ObjectExpression"
    || statement.expression.right.properties.length !== 0
  ) {
    return false;
  }
  return staticMemberPath(statement.expression.left)?.join(".") === expectedPath.join(".");
}

function staticMemberPath(value) {
  const path = [];
  let current = value;
  while (current?.type === "MemberExpression") {
    if (current.computed || current.optional || current.property?.type !== "Identifier") return undefined;
    path.unshift(current.property.name);
    current = current.object;
  }
  if (current?.type !== "Identifier") return undefined;
  path.unshift(current.name);
  return path;
}

function javascriptFiles(directory, allowedRoot) {
  assertDirectory(directory, allowedRoot);
  return readdirSync(directory, { withFileTypes: true }).flatMap((entry) => {
    const path = resolve(directory, entry.name);
    if (entry.isSymbolicLink()) throw new Error(`client build contains a symbolic link: ${path}`);
    if (entry.isDirectory()) return javascriptFiles(path, allowedRoot);
    if (!entry.isFile()) throw new Error(`client build contains a non-regular entry: ${path}`);
    return [".js", ".mjs"].includes(extname(entry.name)) ? [path] : [];
  });
}

function readAstroManifest(serverDir) {
  const chunksDir = resolve(serverDir, "chunks");
  assertDirectory(chunksDir, serverDir);
  const workers = readdirSync(chunksDir, { withFileTypes: true }).flatMap((entry) =>
    entry.isFile() && /^worker-entry_.+\.mjs$/.test(entry.name)
      ? [resolve(chunksDir, entry.name)]
      : []
  );
  if (workers.length !== 1) throw new Error("server build must contain exactly one worker entry");
  const workerPath = workers[0];
  assertRegularFile(workerPath, serverDir);
  const workerSource = readFileSync(workerPath, "utf8");
  const workerProgram = parseJavaScript(workerSource, workerPath);
  const manifestBindings = topLevelVariableBindings(workerProgram, "_manifest");
  if (manifestBindings.length !== 1) {
    throw new Error("server worker must contain exactly one top-level Astro manifest binding");
  }
  const manifestBinding = manifestBindings[0];
  const manifestInitializer = manifestBinding.declaration.init;
  if (
    manifestBinding.kind !== "const"
    || manifestInitializer?.type !== "CallExpression"
    || manifestInitializer.callee?.type !== "Identifier"
    || manifestInitializer.callee.name !== "deserializeManifest"
    || manifestInitializer.arguments?.length !== 1
    || manifestInitializer.arguments[0]?.type !== "ObjectExpression"
  ) {
    throw new Error("server worker Astro manifest binding has an unexpected structure");
  }
  const routesBindings = topLevelVariableBindings(workerProgram, "manifestRoutes");
  if (
    routesBindings.length !== 1
    || routesBindings[0].kind !== "const"
    || !isManifestRoutesReference(routesBindings[0].declaration.init)
  ) {
    throw new Error("server worker must bind manifestRoutes to the parsed Astro manifest");
  }
  const manifestExpression = manifestInitializer.arguments[0];
  const rawManifest = workerSource.slice(manifestExpression.start, manifestExpression.end);
  let manifest;
  try {
    manifest = JSON.parse(rawManifest);
  } catch {
    throw new Error("server worker Astro manifest is not valid JSON");
  }
  return { manifest, workerPath, workerProgram };
}

function parseJavaScript(source, label) {
  try {
    return parseJavaScriptModule(source, {
      allowHashBang: true,
      ecmaVersion: "latest",
      sourceType: "module",
    });
  } catch {
    throw new Error(`${label} is not parseable JavaScript`);
  }
}

function topLevelVariableBindings(program, name) {
  return program.body.flatMap((statement) => {
    if (statement.type !== "VariableDeclaration") return [];
    return statement.declarations.flatMap((declaration) =>
      declaration.id?.type === "Identifier" && declaration.id.name === name
        ? [{ kind: statement.kind, declaration }]
        : []
    );
  });
}

function isManifestRoutesReference(value) {
  return value?.type === "MemberExpression"
    && value.object?.type === "Identifier"
    && value.object.name === "_manifest"
    && (
      (!value.computed && value.property?.type === "Identifier" && value.property.name === "routes")
      || (value.computed && value.property?.type === "Literal" && value.property.value === "routes")
    );
}

function manifestClientJavaScriptAssets(manifest) {
  if (!Array.isArray(manifest?.assets)) throw new Error("server manifest assets must be an array");
  const assets = new Set();
  for (const value of manifest.assets) {
    if (typeof value !== "string" || !value.startsWith(`/${CLIENT_ENTRY_PREFIX}`)) continue;
    if (![".js", ".mjs"].includes(extname(value))) continue;
    const normalized = value.slice(1);
    assertSafeClientAssetPath(normalized);
    if (assets.has(normalized)) throw new Error(`server manifest repeats client asset ${normalized}`);
    assets.add(normalized);
  }
  if (assets.size === 0) throw new Error("server manifest contains no client JavaScript assets");
  return assets;
}

function manifestClientEntryModules(manifest, manifestAssets, clientDir) {
  if (!manifest?.entryModules || typeof manifest.entryModules !== "object" || Array.isArray(manifest.entryModules)) {
    throw new Error("server manifest entryModules must be an object");
  }
  const modules = [];
  for (const [source, asset] of Object.entries(manifest.entryModules)) {
    if (typeof asset !== "string" || !asset.startsWith(CLIENT_ENTRY_PREFIX)) continue;
    assertSafeClientAssetPath(asset);
    if (!manifestAssets.has(asset)) {
      throw new Error(`client entry ${asset} is absent from server manifest assets`);
    }
    const path = resolve(clientDir, asset);
    assertRegularFile(path, clientDir);
    modules.push({ source, asset, path });
  }
  if (modules.length === 0) throw new Error("server manifest contains no client entry modules");
  return modules;
}

function clientAssetPath(clientDir, path) {
  const asset = relative(clientDir, path).split(sep).join("/");
  assertSafeClientAssetPath(asset);
  return asset;
}

function assertSafeClientAssetPath(path) {
  if (
    !path.startsWith(CLIENT_ENTRY_PREFIX)
    || path.startsWith("/")
    || path.split("/").some((part) => part === "" || part === "." || part === "..")
  ) {
    throw new Error(`unsafe client asset path: ${path}`);
  }
}

function assertOnlyExpectedMarkerLiterals(program, expectedMarker, label, required) {
  const markers = [];
  visitJavaScript(program, (node) => {
    if (
      node.type === "Literal"
      && typeof node.value === "string"
      && node.value.startsWith(BUILD_MARKER_PREFIX)
    ) {
      markers.push(node.value);
    }
  });
  if (markers.some((marker) => marker !== expectedMarker)) {
    throw new Error(`${label} contains a stale or inconsistent build identity marker`);
  }
  if (required && markers.length === 0) {
    throw new Error(`${label} does not contain the expected build identity marker literal`);
  }
  return markers;
}

function visitJavaScript(value, visitor) {
  if (!value || typeof value !== "object") return;
  if (Array.isArray(value)) {
    for (const entry of value) visitJavaScript(entry, visitor);
    return;
  }
  if (typeof value.type === "string") visitor(value);
  for (const child of Object.values(value)) visitJavaScript(child, visitor);
}

function assertEqualSets(actual, expected, message) {
  if (
    actual.size !== expected.size
    || [...actual].some((value) => !expected.has(value))
  ) {
    throw new Error(message);
  }
}

function assertDirectory(path, allowedRoot) {
  assertInside(path, allowedRoot);
  const stat = lstatSync(path);
  if (stat.isSymbolicLink() || !stat.isDirectory()) {
    throw new Error(`expected a real directory: ${path}`);
  }
}

function assertRegularFile(path, allowedRoot) {
  assertInside(path, allowedRoot);
  const stat = lstatSync(path);
  if (stat.isSymbolicLink() || !stat.isFile()) {
    throw new Error(`expected a regular file: ${path}`);
  }
}

function assertInside(path, allowedRoot) {
  const root = resolve(allowedRoot);
  const candidate = resolve(path);
  const relativePath = relative(root, candidate);
  if (relativePath === ".." || relativePath.startsWith(`..${sep}`) || relativePath.startsWith(sep)) {
    throw new Error(`path escapes build root: ${path}`);
  }
  let current = root;
  if (lstatSync(current).isSymbolicLink()) {
    throw new Error(`build path contains a symbolic-link ancestor: ${current}`);
  }
  const segments = relativePath.split(sep).filter(Boolean);
  for (const segment of segments.slice(0, -1)) {
    current = resolve(current, segment);
    if (lstatSync(current).isSymbolicLink()) {
      throw new Error(`build path contains a symbolic-link ancestor: ${current}`);
    }
  }
}

if (import.meta.main) verifyBuildArtifacts();
