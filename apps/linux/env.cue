package cuenv

import (
	"github.com/cuenv/cuenv/schema"
	wc "github.com/waddle-social/waddle/ci/contributors"
)

// Waddle for Linux (GTK4 + libadwaita). Its own Cargo workspace and its own
// dev shell (`devShells.<system>.linux`), so GTK never enters the server's
// builds. See claude/linux-client-plan.md.
let _rustInputs = [
	"Cargo.toml",
	"Cargo.lock",
	"rust-toolchain.toml",
	"src/**",
	"data/**",
	schema.#ModuleFiles & {paths: [
		"flake.nix",
		"flake.lock",
		// The shared core and the protocol facade, by path.
		"server/Cargo.toml",
		"server/crates/waddle-client-session/**",
		"server/crates/waddle-xmpp-client-ffi/**",
		"server/crates/waddle-xmpp-client/**",
		"server/crates/waddle-xmpp-core/**",
	]},
]

schema.#Project & {
	name: "waddle-linux"

	runtime: {
		type:   "nix"
		flake:  "../.."
		output: "devShells.x86_64-linux.linux"
	}

	cache: remote: wc.#NamespaceRemoteCache

	ci: providers: ["github"]
	ci: contributors: [
		wc.#Nix,
		wc.#FlakeHubCache,
		wc.#Hestia,
		wc.#NamespaceBazelCache,
		wc.#CuenvRelease,
	]

	ci: provider: github: {
		runner: "namespace-profile-linux-x86"
		runners: arch: {
			"linux-x64": "namespace-profile-linux-x86"
			amd64:       "namespace-profile-linux-x86"
		}
	}

	ci: pipelines: {
		default: {
			when: {
				branch: ["main"]
				defaultBranch: true
				manual:        true
			}
			provider: github: permissions: {contents: "read", "id-token": "write"}
			"tasks": [tasks.fmt, tasks.clippy, tasks.test, tasks.validateData]
		}
		pullRequest: {
			when: {
				pullRequest: true
			}
			provider: github: permissions: {contents: "read", "id-token": "write"}
			"tasks": [tasks.checkCiDrift, tasks.fmt, tasks.clippy, tasks.test, tasks.validateData, tasks.debugCache]
		}
	}

	tasks: {
		checkCiDrift: schema.#Task & {
			command: "cuenv"
			args: ["sync", "ci", "--check", "-p", "."]
			inputs: ["env.cue", schema.#ModuleFiles & {paths: [".github/workflows/waddle-linux-default.yml", ".github/workflows/waddle-linux-pullrequest.yml"]}]
		}

		fmt: schema.#Task & {
			command: "cargo"
			args: ["fmt", "--check"]
			inputs: _rustInputs
		}

		// Same posture as the server: warnings are errors.
		clippy: schema.#Task & {
			command: "cargo"
			args: ["clippy", "--locked", "--all-targets", "--", "-D", "warnings"]
			inputs: _rustInputs
		}

		test: schema.#Task & {
			command: "cargo"
			args: ["test", "--locked"]
			inputs: _rustInputs
		}

		// The desktop entry and AppStream metadata Flathub and software
		// centres read.
		validateData: schema.#Task & {
			command: "bash"
			args: ["-c", "desktop-file-validate data/social.waddle.Waddle.desktop && appstreamcli validate --no-net data/social.waddle.Waddle.metainfo.xml"]
			cache: mode: "read-write"
			hermetic: sandbox: "auto"
			inputs: ["data/**"]
		}

		debugCache: schema.#Task & {
			hermetic: false
			command:  "bash"
			args: ["-c", """
				set -x
				sysctl kernel.apparmor_restrict_unprivileged_userns kernel.unprivileged_userns_clone 2>&1 || true
				unshare -Urn true 2>&1 || true
				sudo -n sysctl -w kernel.apparmor_restrict_unprivileged_userns=0 2>&1 || true
				unshare -Urn true 2>&1 || true
				echo "=== STRICT DEBUG RUN ==="
				export CUENV_CACHE_DIR="/tmp/cuenv-strict-test"
				rm -rf "$CUENV_CACHE_DIR"
				cuenv -L debug task validateData 2>&1 | tail -n 120 || true
				echo "=== INSPECT STRICT ACTION ==="
				cuenv cache stats || true
				for f in $(find "$CUENV_CACHE_DIR" -path '*/ac/*' -type f 2>/dev/null); do
				  h="$(basename "$f")"
				  echo "=== INSPECT $h ==="
				  cuenv cache inspect "$h" || true
				done
				"""]
			inputs: ["env.cue"]
		}
	}
}
