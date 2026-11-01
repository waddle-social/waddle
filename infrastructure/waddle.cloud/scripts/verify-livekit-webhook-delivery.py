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
import uuid

import yaml

INTERNAL_URL = "http://waddle-server.waddle.svc.cluster.local:3000/api/v1/livekit/webhook"
PUBLIC_URL = "https://xmpp.waddle.social/api/v1/livekit/webhook"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--context", required=True, help="Explicit kubectl context to inspect")
    parser.add_argument(
        "--temporary-egress",
        action="store_true",
        help="Create a narrow additive diagnostic policy, then delete it in finally",
    )
    parser.add_argument("--compare-public", action="store_true", help="Also probe the public gateway")
    args = parser.parse_args()
    kubectl = ["kubectl", "--context", args.context, "--request-timeout=15s"]

    def kube_json(*arguments):
        return json.loads(subprocess.check_output([*kubectl, *arguments, "-o", "json"]))

    sfu = kube_json("get", "secret", "-n", "livekit", "livekit-sfu-api-keys")["data"]
    server = kube_json("get", "secret", "-n", "waddle", "waddle-livekit-config")["data"]
    issuer = base64.b64decode(sfu["webhook-key"]).decode()
    keys = yaml.safe_load(base64.b64decode(sfu["keys.yaml"]))
    secret = keys[issuer].encode()
    if not hmac.compare_digest(secret, base64.b64decode(server["LIVEKIT_WEBHOOK_SECRET"])):
        raise RuntimeError("SFU signer and server verifier secrets differ")
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

    name = "webhook-delivery-probe-" + uuid.uuid4().hex[:8]
    policy = {
        "apiVersion": "cilium.io/v2", "kind": "CiliumNetworkPolicy",
        "metadata": {"name": name, "namespace": "livekit"},
        "spec": {
            "endpointSelector": {"matchLabels": {
                "app.kubernetes.io/name": "livekit-sfu",
                "app.kubernetes.io/instance": "livekit-sfu",
            }},
            "egress": [{
                "toEndpoints": [{"matchLabels": {
                    "k8s:io.kubernetes.pod.namespace": "waddle",
                    "k8s:app.kubernetes.io/name": "waddle-server",
                    "k8s:app.kubernetes.io/instance": "waddle-server",
                }}],
                "toPorts": [{"ports": [{"port": "3000", "protocol": "TCP"}]}],
            }],
        },
    }
    created = False
    try:
        if args.compare_public:
            probe("signed-public", PUBLIC_URL, token)
        if args.temporary_egress:
            subprocess.run([*kubectl, "create", "-f", "-"], input=json.dumps(policy), text=True, check=True)
            created = True
        for _ in range(3):
            status = probe("signed-internal", INTERNAL_URL, token)
            if status == 200:
                break
        if status != 200:
            raise RuntimeError("signed internal delivery did not reach the handler")
        if probe("invalid-signature-internal", INTERNAL_URL, "invalid-signature") != 401:
            raise RuntimeError("invalid signature was not rejected")
    finally:
        if created:
            subprocess.run(
                [*kubectl, "delete", "ciliumnetworkpolicy", "-n", "livekit", name, "--wait=true"],
                check=True,
            )


if __name__ == "__main__":
    main()

