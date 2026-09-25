# Chat workspace guidance

- Run chat tests with `bun run test`; run lint with `bun run lint`.
- `bun run lint` builds the WASM package and runs Knip. Keep Knip clean: wire up or remove unused files, exports, and dependencies; do not add broad ignore entries to `knip.json`.
- Add a targeted Knip entry or dependency exception only when required, with a one-line justification in the PR description.
- Pull-request CI runs Knip through the `lint` task in `chat/env.cue`. Preserve that gate when changing chat scripts or CI configuration.
