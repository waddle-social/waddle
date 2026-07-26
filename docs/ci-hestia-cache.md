# Hestia Nix Cache in CI

Waddle's generated Nix-backed GitHub Actions workflows keep their Namespace
runner profiles, but use Hestia rather than a Namespace `/nix` volume for the
Nix binary cache. Each job installs Determinate Nix and then configures Hestia
before running cuenv tasks.

Hestia stores Nix closures through the GitHub Actions cache API. Jobs in one
workflow run union their outputs instead of publishing independent,
last-write-wins `/nix` volume snapshots. The action is pinned to commit
`fb239a2f72d4b6e26eec5425f289dea23b27a527` and downloads Hestia `v2.0.0`.
It filters paths already signed by the upstream Nix cache and gives the
post-job upload up to 900 seconds to drain.

The cache covers Nix store paths such as development shells, toolchains, and
Nix-built packages or images. It does not cache Cargo `target/` outputs, Bun
build outputs, or dependency installs outside the Nix store. Android retains a
separate Namespace path cache for its SDK, NDK, and Gradle directories; that
cache does not include `/nix`.

Ordinary build workflows use the runner-injected Actions cache credentials and
do not need `actions: write`. The repository-owned
`.github/workflows/hestia-cache-gc.yml` is the only Hestia workflow with that
permission. It runs at 03:23 UTC each day, serializes garbage collection, and
supports a manual dry run from the default branch.

Waddle mirrors cuenv's Nix and Hestia contributors in
`ci/contributors/nix.cue`. The Hestia action and the Determinate Nix installer
are pinned to immutable revisions there and in the GC workflow. Keeping the GC
workflow repository-owned avoids running a mutable installer tag with cache
write permission.

A green build does not prove that the post-job cache drain succeeded because
Hestia reports drain failures as warnings. When changing the cache setup,
inspect the Hestia setup and post-job logs, then verify that a later run
substitutes the expected Waddle closure.

Hestia unions concurrent jobs within one workflow run. Waddle has several
project workflows on the same Git ref, so those separate runs can replace the
same root rather than unioning it. Recently pushed paths remain available until
garbage collection, but cache retention and substitution should be monitored
across server, chat, website, colony, cloud, and Android runs.

Regenerate the workflows from the cuenv sources with the cuenv release pinned
by `cue.mod/module.cue`:

```sh
cuenv sync ci -A
```

The separate tag workflow still publishes Waddle releases to FlakeHub. It is a
release destination, not the CI binary cache.
