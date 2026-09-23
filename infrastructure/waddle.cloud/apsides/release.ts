// The server image a release of this program deploys. Flux takes the digest
// from the HelmRelease values the server pipeline rewrites on every publish
// (server/env.cue); under Apsides the same pipeline would rewrite this line,
// then compile, build, sign and deploy a new controller generation.
export const serverImage = "ghcr.io/waddle-social/waddle:main";
