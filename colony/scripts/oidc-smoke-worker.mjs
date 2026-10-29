import { createRequire } from "node:module";
import { readdirSync } from "node:fs";
import { dirname, resolve } from "node:path";

const require = createRequire(import.meta.url);
const { unstable_getMiniflareWorkerOptions } = require("wrangler");
// Use the same pinned runtime as the installed Wrangler CLI, without adding a dependency.
const { Miniflare } = createRequire(require.resolve("wrangler"))("miniflare");
const [configPath, persist, portArgument] = process.argv.slice(2);
const port = Number(portArgument);
const issuer = `http://127.0.0.1:${port}`;
const { workerOptions, main, externalWorkers } =
	unstable_getMiniflareWorkerOptions(configPath);
const modulesRoot = dirname(main);
// The production bundle contains runtime-selected imports. Supply its emitted
// modules explicitly, as Wrangler does for no_bundle, rather than re-parsing them.
const modules = [
	main,
	...readdirSync(modulesRoot, { recursive: true })
		.filter((name) => name.endsWith(".mjs"))
		.map((name) => resolve(modulesRoot, name))
		.filter((path) => path !== main),
].map((path) => ({ type: "ESModule", path }));
const runtime = new Miniflare({
	host: "127.0.0.1",
	port,
	defaultPersistRoot: resolve(persist, "v3"),
	workers: [{ ...workerOptions, modules, modulesRoot }, ...externalWorkers],
});
// Miniflare's public listener routes directly to this workerd instance. Avoid
// Wrangler's separate hot-reload ProxyWorker and its pooled inner HTTP hop.
await runtime.ready;
console.log(`Built Colony worker ready on ${issuer}`);
process.once("SIGTERM", async () => {
	await runtime.dispose();
	process.exit(0);
});
