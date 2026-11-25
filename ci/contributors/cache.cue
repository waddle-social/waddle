package contributors

import "github.com/cuenv/cuenv/schema"

// Nix's per-invocation scratch paths otherwise become part of the action
// key. Confined tasks run with a private /tmp mounted by cuenv.
#NixSandboxScratch: schema.#Environment & {
	NIX_BUILD_TOP: "/tmp"
	TMP:           "/tmp"
	TMPDIR:        "/tmp"
	TEMP:          "/tmp"
	TEMPDIR:       "/tmp"
}

// Shared Namespace Bazel remote cache settings. Default-safe read-only in
// CUE (`upload: false`); #NamespaceBazelCache enables remote uploads via
// $GITHUB_ENV only on trusted `push` events to `refs/heads/main` and isolates
// local cache directories between trusted and untrusted runs.
#NamespaceRemoteCache: schema.#RemoteCache & {
	endpoint: "https://bazel.namespace.invalid"
	protocol: "grpc"
	upload:   false
	auth: credentialHelper: "bazel-credential-nsc"
}

// #NamespaceBazelCache discovers a runner-provisioned Namespace Bazel cache
// (from $BAZELRC, /etc/bazel.bazelrc, or `nsc cache bazel setup`), exports
// CUENV_REMOTE_CACHE* into $GITHUB_ENV, and enforces trusted-vs-untrusted
// upload and local cache directory isolation at runtime. Runners without a
// Namespace Bazel cache explicitly clear CUENV_REMOTE_CACHE so builds fall
// back silently to the local cache without DNS retries.
#NamespaceBazelCache: schema.#Contributor & {
	id: "namespace-bazel-cache"
	tasks: [{
		id:       "namespace-bazel-cache.setup"
		label:    "Setup Namespace Bazel Remote Cache"
		priority: 5
		ci: github: permissions: "id-token": "write"
		script: """
			set -euo pipefail
			: "${GITHUB_ENV:?GITHUB_ENV is required}"
			: "${RUNNER_TEMP:?RUNNER_TEMP is required}"
			if [ "$(uname -s)" != "Linux" ]; then
			  printf 'CUENV_REMOTE_CACHE=\\nCUENV_REMOTE_CACHE_UPLOAD=false\\n' >> "$GITHUB_ENV"
			  exit 0
			fi
			python3 - "$GITHUB_ENV" <<'PY'
			import json, os, re, shutil, subprocess, sys
			from pathlib import Path

			github_env = Path(sys.argv[1])
			trusted = (
			    os.environ.get("GITHUB_EVENT_NAME") == "push"
			    and os.environ.get("GITHUB_REF") == "refs/heads/main"
			)
			runner_temp = os.environ["RUNNER_TEMP"]
			values = {
			    "CUENV_REMOTE_CACHE_UPLOAD": "true" if trusted else "false",
			    "CUENV_CACHE_DIR": os.path.join(
			        runner_temp, "cuenv-cache", "trusted" if trusted else "untrusted"
			    ),
			}

			def normalize_endpoint(endpoint: str) -> str:
			    if endpoint.startswith("grpcs://"):
			        return "https://" + endpoint[len("grpcs://"):]
			    if endpoint.startswith("grpc://"):
			        return "http://" + endpoint[len("grpc://"):]
			    return endpoint

			def parse_bazelrc(path: Path) -> bool:
			    text = path.read_text(encoding="utf-8", errors="replace")
			    endpoint = ""
			    parsed = {}
			    for raw_line in text.splitlines():
			        line = raw_line.split("#", 1)[0].strip()
			        if not line:
			            continue
			        if m := re.search(r"--remote_cache=(\\S+)", line):
			            endpoint = m.group(1)
			        if m := re.search(r"--tls_certificate=(\\S+)", line):
			            parsed["CUENV_REMOTE_CACHE_CA_CERT"] = m.group(1)
			        if m := re.search(r"--tls_client_certificate=(\\S+)", line):
			            parsed["CUENV_REMOTE_CACHE_CLIENT_CERT"] = m.group(1)
			        if m := re.search(r"--tls_client_key=(\\S+)", line):
			            parsed["CUENV_REMOTE_CACHE_CLIENT_KEY"] = m.group(1)
			        if m := re.search(r"--credential_helper=[^=\\s]+=(\\S+)", line):
			            parsed["CUENV_REMOTE_CACHE_CREDENTIAL_HELPER"] = m.group(1)
			    if not endpoint:
			        return False
			    values["CUENV_REMOTE_CACHE"] = normalize_endpoint(endpoint)
			    values["CUENV_REMOTE_CACHE_PROTOCOL"] = "grpc"
			    values.update(parsed)
			    return True

			configured = False
			for candidate in (os.environ.get("BAZELRC", ""), "/etc/bazel.bazelrc"):
			    if candidate:
			        p = Path(candidate)
			        if p.is_file():
			            configured = parse_bazelrc(p)
			            if configured:
			                break

			if not configured and shutil.which("nsc"):
			    try:
			        proc = subprocess.run(
			            ["nsc", "cache", "bazel", "setup", "--disable_build_events", "-o", "json"],
			            capture_output=True,
			            text=True,
			            timeout=20,
			            check=True,
			        )
			        data = json.loads(proc.stdout)
			        endpoint = (data.get("endpoint") or "").strip()
			        if endpoint:
			            values["CUENV_REMOTE_CACHE"] = normalize_endpoint(endpoint)
			            values["CUENV_REMOTE_CACHE_PROTOCOL"] = "grpc"
			            if data.get("server_ca_cert"):
			                values["CUENV_REMOTE_CACHE_CA_CERT"] = data["server_ca_cert"]
			            if data.get("client_cert") and data.get("client_key"):
			                values["CUENV_REMOTE_CACHE_CLIENT_CERT"] = data["client_cert"]
			                values["CUENV_REMOTE_CACHE_CLIENT_KEY"] = data["client_key"]
			            if data.get("credential_helper_domains"):
			                helper = shutil.which("bazel-credential-nsc")
			                if helper:
			                    values["CUENV_REMOTE_CACHE_CREDENTIAL_HELPER"] = helper
			            configured = True
			    except Exception:
			        pass

			if not configured:
			    values["CUENV_REMOTE_CACHE"] = ""

			with github_env.open("a", encoding="utf-8") as handle:
			    for k, v in values.items():
			        if "\\n" in v or "\\r" in v:
			            raise SystemExit(f"Invalid newline in {k}")
			        handle.write(f"{k}={v}\\n")
			PY
			"""
	}, {
		id:       "namespace-bazel-cache.status"
		label:    "Check Namespace Bazel Remote Cache"
		priority: 15
		script: """
			set -euo pipefail
			if [ -z "${CUENV_REMOTE_CACHE:-}" ]; then
			  echo "Namespace Bazel remote cache not configured on this runner; using local cache only."
			  exit 0
			fi
			cuenv cache status || true
			"""
	}]
}
