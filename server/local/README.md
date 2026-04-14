# Local Server Dev

`server/local/` holds the checked-in local example config and certs plus ignored runtime data used by `cuenv task dev`.

## Files

- `waddle.env.example`: template for the local runtime config
- `waddle.env`: ignored local runtime config used directly by `cuenv task dev`
- `certs/`: self-signed TLS certs for local XMPP startup (gitignored; generate with `server/scripts/generate-local-certs.sh`)
- `data/`: local SQLite database files
- `uploads/`: local upload storage

## Usage

1. Copy `local/waddle.env.example` to `local/waddle.env`.
2. Edit `local/waddle.env` and update `WADDLE_AUTH_PROVIDERS_JSON` for your issuer/domain.
3. Generate local TLS certs: `./scripts/generate-local-certs.sh`
4. Run `cuenv task dev`.

For local `chat`, point `SERVER_BASE_URL` at `http://localhost:3000`.
