# waddle.cloud GitOps OCI Bundle

This directory is the source for the Flux OCI bootstrap artifact pushed to:

- `oci://ghcr.io/waddle-social/waddle/gitops:latest`

Render the bundle locally before publishing:

```bash
kubectl kustomize infrastructure/waddle.cloud/gitops
```

Publish it manually:

```bash
echo "$GITHUB_TOKEN" | docker login ghcr.io -u YOUR_GITHUB_USERNAME --password-stdin

flux push artifact oci://ghcr.io/waddle-social/waddle/gitops:latest \
  --path="./infrastructure/waddle.cloud/gitops" \
  --source="$(git config --get remote.origin.url)" \
  --revision="$(git rev-parse --short HEAD)"
```

Verify the published artifact:

```bash
flux pull artifact oci://ghcr.io/waddle-social/waddle/gitops:latest \
  --output /tmp/waddle-gitops-artifact

find /tmp/waddle-gitops-artifact -maxdepth 2 -type f | sort
```
