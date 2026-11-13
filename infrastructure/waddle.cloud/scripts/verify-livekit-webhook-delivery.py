#!/usr/bin/env python3
"""Probe signed webhook delivery from an existing SFU pod without call effects."""

import argparse
import base64
import hashlib
import hmac
import json
import re
import shlex
import subprocess
import time

import yaml

INTERNAL_URL = "http://waddle-server.waddle.svc.cluster.local:3000/api/v1/livekit/webhook"
PUBLIC_URL = "https://xmpp.waddle.social/api/v1/livekit/webhook"


def kubectl_for_context(requested):
    if not requested.isprintable() or requested.startswith("-"):
        raise SystemExit("invalid Kubernetes context")
    contexts = subprocess.check_output(
        ["kubectl", "config", "get-contexts", "-o", "name"], text=True,
    ).splitlines()
    for context in contexts:
        if context == requested:
            return ["kubectl", "--context=" + context, "--request-timeout=15s"]
    raise SystemExit("Kubernetes context is not in the local inventory")


def load_webhook_credentials(kube_json):
    try:
        sfu = kube_json("get", "secret", "-n", "livekit", "livekit-sfu-api-keys")["data"]
        server = kube_json("get", "secret", "-n", "waddle", "waddle-livekit-config")["data"]
        issuer = base64.b64decode(sfu["webhook-key"], validate=True).decode()
        keys = yaml.safe_load(base64.b64decode(sfu["keys.yaml"], validate=True))
        if not issuer or not isinstance(keys, dict) or not isinstance(keys.get(issuer), str):
            raise ValueError("invalid signer mapping")
        secret = keys[issuer].encode()
        verifier_secret = base64.b64decode(server["LIVEKIT_WEBHOOK_SECRET"], validate=True)
        if not secret or not verifier_secret:
            raise ValueError("empty webhook secret")
    except (KeyError, TypeError, ValueError, yaml.YAMLError):
        # YAML parser diagnostics can embed source lines containing secrets.
        # Suppress both their message and chained traceback at the CLI boundary.
        raise SystemExit("webhook credentials are malformed or incomplete") from None
    if not hmac.compare_digest(secret, verifier_secret):
        raise RuntimeError("SFU signer and server verifier secrets differ")
    return issuer, secret


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--context", required=True, help="Explicit kubectl context to inspect")
    parser.add_argument("--compare-public", action="store_true", help="Also probe the public gateway")
    args = parser.parse_args()
    kubectl = kubectl_for_context(args.context)

    def kube_json(*arguments):
        return json.loads(subprocess.check_output([*kubectl, *arguments, "-o", "json"]))

    issuer, secret = load_webhook_credentials(kube_json)
    pods = kube_json(
        "get", "pods", "-n", "livekit", "-l",
        "app.kubernetes.io/name=livekit-sfu,app.kubernetes.io/instance=livekit-sfu",
    )["items"]
    ready = [
        pod["metadata"]["name"] for pod in pods
        if not pod["metadata"].get("deletionTimestamp")
        and any(condition["type"] == "Ready" and condition["status"] == "True"
                for condition in pod.get("status", {}).get("conditions", []))
    ]
    if not ready:
        raise RuntimeError("no ready SFU pod")

    now = int(time.time())
    # Unknown event + no room, participant or ID: accepted telemetry only, no
    # call teardown, persistent deduplication row or application side effect.
    body = json.dumps(
        {"event": "waddle_delivery_probe", "createdAt": str(now)},
        separators=(",", ":"),
    ).encode()

    def encode(data):
        return base64.urlsafe_b64encode(data).rstrip(b"=")

    claims = {
        "iss": issuer, "iat": now, "nbf": now - 5, "exp": now + 120,
        "sha256": base64.b64encode(hashlib.sha256(body).digest()).decode(),
    }
    unsigned = b".".join([
        encode(b'{"alg":"HS256","typ":"JWT"}'),
        encode(json.dumps(claims, separators=(",", ":")).encode()),
    ])
    token = (unsigned + b"." + encode(hmac.new(secret, unsigned, hashlib.sha256).digest())).decode()

    def probe(label, url, credential):
        # Pass the JWT to kubectl through stdin, not local argv or printed output.
        command = (
            "wget -T 8 -S -O - --header="
            + shlex.quote("Authorization: " + credential)
            + " --header=" + shlex.quote("Content-Type: application/webhook+json")
            + " --post-data=" + shlex.quote(body.decode())
            + " " + shlex.quote(url) + " 2>&1\n"
        )
        result = subprocess.run(
            [*kubectl, "exec", "-i", "-n", "livekit", ready[0], "--", "sh"],
            input=command, text=True, stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT, timeout=20, check=False,
        )
        statuses = re.findall(r"HTTP/[\d.]+\s+(\d{3})", result.stdout)
        status = int(statuses[-1]) if statuses else None
        print(json.dumps({"probe": label, "http_status": status, "exec_exit": result.returncode}), flush=True)
        return status

    if args.compare_public:
        probe("signed-public", PUBLIC_URL, token)
    if probe("signed-internal", INTERNAL_URL, token) != 200:
        raise RuntimeError("signed internal delivery did not reach the handler")
    if probe("invalid-signature-internal", INTERNAL_URL, "invalid-signature") != 401:
        raise RuntimeError("invalid signature was not rejected")


if __name__ == "__main__":
    main()
