# VAPID bootstrap key

Web Push (RFC 8292) needs a stable ECDSA P-256 private key. The server seals
that key in the database. `WADDLE_VAPID_PRIVATE_KEY` is only the optional
bootstrap input used the first time a deployment has no stored key.

## What the process does

On boot, `VapidStorage::load_or_provision` chooses one path:

1. If `WADDLE_VAPID_PRIVATE_KEY` is set, parse it, initialize the signer, and
   persist the sealed key. Any parse, signer, or persistence failure fails
   boot and leaves no stored key. The variable stays set.
2. Otherwise, load the latest non-retired sealed key.
3. Otherwise, generate a fresh P-256 key and persist it.

The running process does not clear the variable. Database and telemetry
threads may already be reading the environment, and clearing it would not
erase the original deployment environment or other copies. Decoded temporary
key material is zeroized. The persisted private key stays sealed at rest.

The value is the raw 32-byte P-256 scalar, base64url-encoded without padding.
Treat it as a secret: the running process and anyone who can inspect its
environment or the deployment spec are inside the key's trust boundary.

There is no production child-process or exec path that inherits this
environment in order to relaunch the server. A future child launcher must
build an explicit environment that excludes `WADDLE_VAPID_PRIVATE_KEY`,
`LISTEN_FDS`, and `LISTEN_FD_NAMES` (for example with `Command::env_remove`),
unless it is a separately reviewed listener handoff. Do not mutate the parent
environment to configure a child.

## After the first successful boot

Once a pod has started successfully and the key is stored, remove
`WADDLE_VAPID_PRIVATE_KEY` from the deployment. Later boots load the sealed
key. Leaving the bootstrap variable in the spec keeps the private key in
every new pod's environment.

1. Confirm the process became ready after the boot that persisted the key.
   A failed boot did not store a key; keep the variable until a boot succeeds.
2. Delete `WADDLE_VAPID_PRIVATE_KEY` from the Deployment, Helm values, secret,
   or other env source.
3. Roll the workload so new processes start without the variable.
4. Confirm the new pods become ready and Web Push still signs. They are using
   the stored key.

Do this for every replica source. A chart upgrade or secret refresh that puts
the variable back reintroduces the bootstrap secret.

### Helm

The chart has no dedicated VAPID value. Inject the bootstrap secret with
`extraSecretRefs` or `containerExtraEnv`, then remove that key after the
first successful boot:

- `extraSecretRefs` mounts an external Secret through `envFrom`. Drop
  `WADDLE_VAPID_PRIVATE_KEY` from that Secret and roll the pods. Set
  `extraSecretChecksum` when the Secret is managed outside the chart so the
  rollout sees the change.
- `containerExtraEnv` / `config.extraEnv` can also carry the variable. Delete
  the entry and upgrade.

Do not put the private key in `config.extraEnv`. That field is rendered into
a ConfigMap.

### Containers and other process managers

Pass `-e WADDLE_VAPID_PRIVATE_KEY=...` (or the equivalent unit/environment
file) only for the first start. After that start is ready, remove the
variable from the unit, compose file, or orchestrator spec and restart.
