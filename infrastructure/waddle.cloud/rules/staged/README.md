# Staged alert rules

Rules here are written and linted but not evaluated anywhere. The sync in
`../../env.cue` uploads `rules/mimir/*.yaml` and `rules/loki/*.yaml` only;
nothing under `staged/` reaches a ruler.

A rule is staged when its series cannot be confirmed in production yet:
the instrument is new, or it is declared without a call site. A
missing-series rule for such a metric would fire the moment it was
uploaded.

To promote a group:

1. Confirm in Grafana that every series its expressions read exists under
   the name the rule uses. The names in `mimir/waddle-authority.yaml` are
   derived from the OTLP to Prometheus translation
   (`UnderscoreEscapingWithSuffixes`), not observed; see
   `docs/operations/server-metrics.md`.
2. Replace the thresholds marked "not measured" with measured ones.
3. Move the group into `../mimir/waddle-reliability.yaml` and its tests
   into `../tests/`, where `../tests/run.sh` evaluates them and the sync
   publishes them.

Checks:

```sh
sed '/^namespace:/d' staged/mimir/waddle-authority.yaml > /tmp/waddle-authority.yaml
promtool check rules /tmp/waddle-authority.yaml
cp staged/tests/authority.test.yaml /tmp/ && promtool test rules /tmp/authority.test.yaml
```

`../tests/run.sh` runs the first check for every staged rule file. The
unit tests in `staged/tests/` are not part of `run.sh`.
