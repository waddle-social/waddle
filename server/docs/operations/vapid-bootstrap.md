# VAPID bootstrap key

Web Push (RFC 8292) needs a stable ECDSA P-256 private key. The server seals
that key in the database. `WADDLE_VAPID_PRIVATE_KEY` is only the optional
bootstrap input used the first time a deployment has no stored key.

## What the process does

On PostgreSQL, `VapidStorage::load_or_provision` selects the key in one
transaction under the catalog and Push schema fences:

1. If exactly one active key exists, authenticate and reuse it. If the
   environment variable is set, its public key must match the stored key.
2. If no key has ever been stored, use the environment key when supplied,
   otherwise generate a fresh P-256 key and persist it.
3. Fail closed for malformed input, a mismatched key, multiple active keys,
   retired-only history, invalid stored material, or inconsistent markers.
   Adding another row is not a rotation procedure.

The direct PostgreSQL provisioning API verifies existing Push or VAPID
markers against the live cluster identity. When either marker exists, its
database role must be able to execute `pg_catalog.pg_control_system()`;
otherwise provisioning fails closed without changing the marker or key.
Unmarked raw bootstrap does not need this identity-read privilege. Follow the
database-lineage role setup for the grant; this API never grants it itself.

The signer is returned only after the database acknowledges COMMIT. A lost
acknowledgement leaves the commit outcome indeterminate. Resolve the original
primary and backend, or establish that a retry uses the same fenced primary
and durable lineage. A retry on that primary reacquires both fences and reads
durable state before deciding whether to create a key. This API does not
establish continuity across calls or reconcile a lossy failover to a new
primary. Socket closure alone does not prove the original backend released its
locks. A failed boot alone does not prove that no key was stored. SQLite
retains its legacy behavior: an environment key is persisted when supplied;
otherwise the latest active key is loaded or a key generated.

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
   Keep the variable until a boot succeeds; an unacknowledged commit can have
   stored the key even if that boot failed.
2. Delete `WADDLE_VAPID_PRIVATE_KEY` from the Deployment, Helm values, secret,
   or other env source.
3. Roll the workload so new processes start without the variable.
4. Confirm the new pods become ready and Web Push still signs. They are using
   the stored key.

Do this for every replica source. A chart upgrade or secret refresh that puts
the variable back reintroduces the bootstrap secret.

### Helm

The chart has no dedicated VAPID value. `WADDLE_VAPID_PRIVATE_KEY` comes
only from a Kubernetes Secret. After the first successful boot, remove the
variable from that Secret and roll the pods:

- `extraSecretRefs` mounts an external Secret through `envFrom`. Drop
  `WADDLE_VAPID_PRIVATE_KEY` from that Secret. Set `extraSecretChecksum`
  when the Secret is managed outside the chart so the rollout sees the
  change.
- `containerExtraEnv` may reference the same Secret with
  `valueFrom.secretKeyRef`. Delete that entry and upgrade once the key is
  stored.

`config.extraEnv` is rendered into a ConfigMap. Do not put
`WADDLE_VAPID_PRIVATE_KEY` there.

### Containers and other process managers

Pass `-e WADDLE_VAPID_PRIVATE_KEY=...` (or the equivalent unit/environment
file) only for the first start. After that start is ready, remove the
variable from the unit, compose file, or orchestrator spec and restart.
