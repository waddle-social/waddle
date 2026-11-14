# Staged alert rules

Nothing is staged. Rule files placed in `staged/mimir/` are syntax-checked
by `../tests/run.sh` but not evaluated anywhere: the sync in `../../env.cue`
uploads `rules/mimir/*.yaml` and `rules/loki/*.yaml` only, and unit tests in
`staged/tests/` are not part of `run.sh`.

Stage a rule when a missing-series alert would read a series that is not
exported yet (a new instrument, or one declared without a call site): such
an alert fires the moment it is uploaded. A threshold alert on a series
that does not exist yet is inert rather than wrong and can go straight into
`../mimir/`.

To promote a group:

1. Confirm the name each expression reads. For a `waddle_clustering_*`
   series, add it to
   `server/crates/waddle-server/src/clustering/alert_metric_contract.json`,
   which `../tests/push_otlp_contract.py` checks through Prometheus's real
   OTLP translator; for any other series, compare the instrument's name,
   unit and kind with `docs/operations/server-metrics.md` and, after
   deployment, with Grafana.
2. Replace the thresholds marked "not measured" with measured ones once
   there is production data.
3. Move the group into `../mimir/waddle-reliability.yaml` and its tests
   into `../tests/`, where `../tests/run.sh` evaluates them and the sync
   publishes them.

Checks for a staged file:

```sh
sed '/^namespace:/d' staged/mimir/<file>.yaml > /tmp/<file>.yaml
promtool check rules /tmp/<file>.yaml
```
