# Local Server Dev

`server/local/` holds the checked-in local example config and certs plus ignored runtime data used by `cuenv task dev`.

## Files

- `waddle.env.example`: template for the local runtime config
- `waddle.env`: ignored local runtime config used directly by `cuenv task dev`
- `certs/`: checked-in self-signed certs for local XMPP startup
- `data/`: local SQLite database files
- `uploads/`: local upload storage

## Usage

1. Copy `local/waddle.env.example` to `local/waddle.env`.
2. Edit `local/waddle.env` and replace the `replace-me` secret inside `WADDLE_AUTH_PROVIDERS_JSON`.
3. Run `cuenv task dev`.

For local `chat`, point `SERVER_BASE_URL` at `http://localhost:3000`.
