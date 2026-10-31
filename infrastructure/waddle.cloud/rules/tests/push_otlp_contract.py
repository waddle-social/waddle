#!/usr/bin/env python3
"""Exercise real OTLP name translation and the checked-in push recording rules.

Needs Python's standard library and an existing Prometheus binary (3.x).
Run with --prometheus /path/to/prometheus; no services or credentials needed.
The Rust exported-metric tests bind both shared fixtures (push gauges and the
clustering instruments the alert rules read) to production names and units.
Alloy forwards OTLP unchanged to Grafana Cloud, whose default suffix contract
matches UnderscoreEscapingWithSuffixes:
https://grafana.com/docs/grafana-cloud/send-data/otlp/otlp-format-considerations/
"""

import argparse
import json
import pathlib
import socket
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request

sys.dont_write_bytecode = True
from alert_metric_contract import exercise_parser_controls, validate_rule_contract


def read_json(url, payload=None):
    request = urllib.request.Request(
        url,
        data=None if payload is None else json.dumps(payload).encode(),
        headers={"Content-Type": "application/json"},
    )
    with urllib.request.urlopen(request, timeout=2) as response:
        body = response.read()
        return json.loads(body) if body else {}


def run(prometheus):
    repo = pathlib.Path(__file__).resolve().parents[4]
    contract = json.loads((repo / "server/crates/waddle-xmpp/src/telemetry/push_gauge_contract.json").read_text())
    cluster_contract = json.loads((repo / "server/crates/waddle-server/src/clustering/alert_metric_contract.json").read_text())
    with tempfile.TemporaryDirectory(prefix="waddle-push-otlp-") as directory:
        work = pathlib.Path(directory)
        # Every Mimir rule file, like run.sh; an empty set is a failure.
        rule_paths = sorted((repo / "infrastructure/waddle.cloud/rules/mimir").glob("*.yaml"))
        if not rule_paths:
            raise RuntimeError("no Mimir rule files under infrastructure/waddle.cloud/rules/mimir")
        for rule_path in rule_paths:
            rules = rule_path.read_text()
            (work / rule_path.name).write_text("\n".join(line for line in rules.splitlines() if not line.startswith("namespace:")))
        (work / "prometheus.yaml").write_text(
            "global:\n  evaluation_interval: 1s\n"
            "otlp:\n  translation_strategy: UnderscoreEscapingWithSuffixes\n"
            "rule_files:\n" + "".join(f"  - {rule_path.name}\n" for rule_path in rule_paths)
        )
        with socket.socket() as reservation:
            reservation.bind(("127.0.0.1", 0))
            port = reservation.getsockname()[1]
        base = f"http://127.0.0.1:{port}"
        with (work / "prometheus.log").open("w+") as log:
            process = subprocess.Popen(
                [prometheus, "--config.file=prometheus.yaml", "--storage.tsdb.path=data",
                 f"--web.listen-address=127.0.0.1:{port}", "--web.enable-otlp-receiver"],
                cwd=work, stdout=log, stderr=subprocess.STDOUT,
            )
            try:
                deadline = time.monotonic() + 15
                while True:
                    if process.poll() is not None:
                        raise RuntimeError("Prometheus exited before becoming ready")
                    try:
                        with urllib.request.urlopen(base + "/-/ready", timeout=2) as ready:
                            ready.read()
                        break
                    except (urllib.error.URLError, TimeoutError):
                        if time.monotonic() >= deadline:
                            raise RuntimeError("Prometheus startup exceeded 15 seconds")
                        time.sleep(0.1)
                # Use the loaded rule set and Prometheus's parser rather than
                # tokenizing YAML or PromQL: literals, comments and label names
                # are not evidence that a rule reads a metric.
                def parse_query(expression):
                    url = base + "/api/v1/parse_query?" + urllib.parse.urlencode({"query": expression})
                    result = read_json(url)
                    if result.get("status") != "success" or not isinstance(result.get("data"), dict):
                        raise AssertionError(f"unexpected PromQL parser response: {result}")
                    return result["data"]

                expected_files = {rule_path.name for rule_path in rule_paths}
                deadline = time.monotonic() + 10
                while True:
                    result = read_json(base + "/api/v1/rules")
                    if result.get("status") != "success" or not isinstance(result.get("data", {}).get("groups"), list):
                        raise AssertionError(f"unexpected rules response: {result}")
                    groups = result["data"]["groups"]
                    loaded_files = {pathlib.Path(group["file"]).name for group in groups}
                    if loaded_files == expected_files:
                        break
                    if time.monotonic() >= deadline:
                        raise AssertionError(f"rule files not loaded: expected={sorted(expected_files)} actual={sorted(loaded_files)}")
                    time.sleep(0.1)
                exercise_parser_controls(parse_query)
                validate_rule_contract(groups, cluster_contract, parse_query)
                print("PASS clustering alert selectors match the metric contract")

                timestamp = time.time_ns()
                metrics = []
                expected = {}
                for entry in contract:
                    value = timestamp // 1_000_000_000 if entry["value"] == "now" else entry["value"]
                    metrics.append({
                        "name": entry["name"], "unit": entry["unit"],
                        "gauge": {"dataPoints": [{
                            "timeUnixNano": str(timestamp), "asInt": str(value),
                            "attributes": [{"key": key, "value": {"stringValue": val}}
                                           for key, val in entry["attributes"].items()],
                        }]},
                    })
                    expected[entry["prometheus_name"]] = value
                    expected[entry["recording_name"]] = value
                # Clustering instruments are f64 gauges; the translated name
                # must be the one the alert rules read, and no other spelling.
                cluster_expected = set()
                unexported = set()
                for entry in cluster_contract:
                    metrics.append({
                        "name": entry["name"], "unit": entry["unit"],
                        "gauge": {"dataPoints": [{
                            "timeUnixNano": str(timestamp), "asDouble": float(entry["value"]),
                        }]},
                    })
                    expected[entry["prometheus_name"]] = entry["value"]
                    cluster_expected.add(entry["prometheus_name"])
                    unexported.update(entry["unexported_rule_spellings"])
                # A negative control proves suffix translation really ran.
                metrics.append({
                    "name": "waddle.translation.dimensionless", "unit": "1",
                    "gauge": {"dataPoints": [{"timeUnixNano": str(timestamp), "asInt": "7"}]},
                })
                expected["waddle_translation_dimensionless_ratio"] = 7
                response = read_json(base + "/api/v1/otlp/v1/metrics", {
                    "resourceMetrics": [{"resource": {"attributes": [
                        {"key": "service.name", "value": {"stringValue": "push-contract"}},
                        {"key": "service.instance.id", "value": {"stringValue": "one"}},
                    ]}, "scopeMetrics": [{"scope": {"name": "waddle"}, "metrics": metrics}]}],
                })
                if response.get("partialSuccess"):
                    raise RuntimeError(f"OTLP receiver rejected points: {response}")
                selector = '{__name__=~"xmpp_push_.*|waddle_push_.*|waddle_translation_.*|waddle_clustering_.*"}'
                query = base + "/api/v1/query?" + urllib.parse.urlencode({"query": selector})
                deadline = time.monotonic() + 10
                while True:
                    result = read_json(query)
                    actual = {sample["metric"]["__name__"]: float(sample["value"][1])
                              for sample in result["data"]["result"]}
                    if all(actual.get(name) == value for name, value in expected.items()):
                        cluster_actual = {name for name in actual if name.startswith("waddle_clustering_")}
                        if cluster_actual != cluster_expected or cluster_actual & unexported:
                            raise AssertionError(
                                f"clustering series mismatch\nexpected={sorted(cluster_expected)}\n"
                                f"actual={sorted(cluster_actual)}\nunexported={sorted(unexported)}")
                        break
                    if time.monotonic() >= deadline:
                        raise AssertionError(f"translated series/recording rule mismatch\nexpected={expected}\nactual={actual}")
                    time.sleep(0.1)
                for name in sorted(expected):
                    print(f"PASS {name}={actual[name]:g}")
                for name in sorted(unexported):
                    print(f"PASS {name} absent")
            except BaseException:
                log.flush()
                log.seek(0)
                print(log.read())
                raise
            finally:
                process.terminate()
                try:
                    process.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=3)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--prometheus", required=True, help="existing Prometheus 3.x binary")
    run(parser.parse_args().prometheus)
