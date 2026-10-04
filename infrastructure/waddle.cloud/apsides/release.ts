// Values the server publish pipeline (server/env.cue) rewrites before Flux
// sees them: the image tag and digest, the extension modules and
// WADDLE_GIT_SHA. They mirror the checked-in HelmRelease values, so this
// program, like that file, deploys with extensions disabled until a publish
// fills them in. Under Apsides the pipeline would rewrite this module, then
// compile, build, sign and deploy a new controller generation.
export const serverImage = "ghcr.io/waddle-social/waddle:main";
export const gitSha: string | undefined = undefined;
export const extensions = { cacheDir: "/var/lib/waddle/extensions", enabled: false, modules: [] as unknown[] };
