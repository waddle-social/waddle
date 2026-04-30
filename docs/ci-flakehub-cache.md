# FlakeHub Cache in CI

Waddle's Nix-backed GitHub Actions workflows install Determinate Nix and configure FlakeHub Cache before running Cuenv tasks.

The cache is configured for the `waddle-social/waddle` FlakeHub flake. It accelerates Nix store paths such as the development shell, toolchains, and Nix-built package or image outputs. It does not cache Cargo `target/` outputs, Bun build outputs, or dependency installs outside the Nix store.

Trusted cache-enabled workflows need GitHub OIDC:

```yaml
permissions:
  contents: read
  id-token: write
```

The generated workflows may include additional permissions for checks, packages, or deployments. Do not remove those when editing the Cuenv source.

Pull request workflows grant `id-token: write` so same-repository pull requests can authenticate to FlakeHub Cache. Pull requests from forks still run the cache setup step, but FlakeHub Cache is unavailable for those fork workflows because GitHub's JSON Web Token claims cannot authenticate them to FlakeHub Cache.

Regenerate GitHub Actions workflows from the Cuenv sources with `cuenv sync ci -A`.

For local machines, install Determinate Nix and authenticate once:

```sh
determinate-nixd login
```

After login, local Nix builds can pull cached paths from FlakeHub Cache when those paths have been produced by CI or another authenticated builder.
