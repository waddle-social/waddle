package cuenv

import (
	"list"

	"github.com/cuenv/cuenv/schema"
	c "github.com/cuenv/cuenv/contrib/contributors"
)

let _NamespaceNix = schema.#Contributor & {
	id: "namespaceNix"
	when: runtimeType: ["nix"]
	tasks: [
		{
			id:       "nix.cache"
			label:    "Cache /nix on Namespace volume"
			priority: 0
			provider: github: {
				uses: "namespacelabs/nscloud-cache-action@v1"
				with: cache: "nix"
			}
		},
		{
			id:       "nix.chown"
			label:    "Hand /nix to the runner user"
			priority: 1
			dependsOn: ["nix.cache"]
			script: "sudo chown -R runner /nix"
		},
		{
			id:       "nix.install"
			label:    "Install Nix"
			priority: 2
			dependsOn: ["nix.chown"]
			provider: github: {
				uses: "cachix/install-nix-action@v31"
				with: extra_nix_config: "accept-flake-config = true"
			}
		},
	]
}

// Caches the sdkmanager-provisioned Android SDK/NDK and the Gradle caches
// across runs; without it every CI run re-downloads ~2 GB of SDK packages.
let _NamespaceAndroidCache = schema.#Contributor & {
	id: "namespaceAndroidCache"
	when: runtimeType: ["nix"]
	tasks: [
		{
			id:       "android.cache"
			label:    "Cache Android SDK and Gradle on Namespace volume"
			priority: 0
			provider: github: {
				uses: "namespacelabs/nscloud-cache-action@v1"
				with: path: "/home/runner/.android-sdk\n/home/runner/.gradle"
			}
		},
	]
}

// Shared Rust FFI surface: any change to the client crates retriggers the
// Android pipelines, mirroring the hand-maintained Apple workflow filters.
let _ffiInputs = [
	"../../server/Cargo.toml",
	"../../server/Cargo.lock",
	"../../server/rust-toolchain.toml",
	"../../server/crates/waddle-xmpp-client/**",
	"../../server/crates/waddle-xmpp-client-ffi/**",
	"../../scripts/build-android-rust.sh",
	"../../scripts/check-android-ffi-bindings.sh",
]

let _gradleInputs = [
	"settings.gradle.kts",
	"build.gradle.kts",
	"gradle.properties",
	"gradle/**",
	"gradlew",
	"app/**",
	"core/client/build.gradle.kts",
	"core/client/consumer-rules.pro",
	"core/client/src/main/kotlin/**",
	"debug.keystore",
]

schema.#Project & {
	name: "waddle-android"

	runtime: {
		type:  "nix"
		flake: "../.."
	}

	ci: providers: ["github"]
	ci: contributors: [
		_NamespaceNix,
		_NamespaceAndroidCache,
		c.#CuenvRelease,
	]

	ci: provider: github: {
		runner: "namespace-profile-linux-x86"
		runners: arch: {
			"linux-x64":    "namespace-profile-linux-x86"
			"darwin-arm64": "namespace-profile-darwin-arm64"
			amd64:          "namespace-profile-linux-x86"
		}
	}

	ci: pipelines: {
		default: {
			when: {
				branch: ["main"]
				defaultBranch: true
				manual:        true
			}
			provider: github: permissions: contents: "write"
			"tasks": [tasks.checkBindingsDrift, tasks.test, tasks.publishDebugApk]
		}
		pullRequest: {
			when: {
				pullRequest: true
			}
			provider: github: permissions: contents: "read"
			"tasks": [tasks.checkCiDrift, tasks.checkBindingsDrift, tasks.test, tasks.build]
		}
		// Manual probe: verifies /dev/kvm on the Namespace runner and runs
		// the Gradle-managed-device instrumented smoke suite on a headless
		// ATD emulator. Promoted into pullRequest once proven stable.
		deviceTests: {
			when: manual: true
			provider: github: permissions: contents: "read"
			"tasks": [tasks.gmdCheck]
		}
	}

	tasks: {
		// Provisions the pinned SDK packages (cmdline-tools, platforms,
		// build-tools, NDK) and writes local.properties. Idempotent.
		setupSdk: schema.#Task & {
			command: "bash"
			args: ["../../scripts/setup-android-sdk.sh"]
			inputs: ["../../scripts/setup-android-sdk.sh"]
		}

		// Cross-compiles libwaddle_xmpp_client_ffi.so for arm64-v8a and
		// x86_64 into the gitignored jniLibs tree (debug profile: the PR
		// pipeline only proves the build; release .so's are built by the
		// release pipeline once signing lands).
		buildRustJni: schema.#Task & {
			command: "bash"
			args: ["-c", "../../scripts/build-android-rust.sh --debug --skip-bindings"]
			dependsOn: [setupSdk]
			inputs: _ffiInputs
			outputs: ["core/client/src/main/jniLibs/**"]
		}

		// Fails when the committed Kotlin UniFFI bindings drift from the
		// Rust FFI crate (host-only, no SDK needed).
		checkBindingsDrift: schema.#Task & {
			command: "bash"
			args: ["../../scripts/check-android-ffi-bindings.sh"]
			inputs: list.Concat([_ffiInputs, ["core/client/src/main/kotlin/social/waddle/client/ffi/**"]])
		}

		checkCiDrift: schema.#Task & {
			command: "cuenv"
			args: ["sync", "ci", "--check", "-A"]
			inputs: ["env.cue", "../../.github/workflows/waddle-android-*.yml"]
		}

		build: schema.#Task & {
			command: "bash"
			args: ["-c", "./gradlew --no-daemon :app:assembleDebug"]
			dependsOn: [setupSdk, buildRustJni]
			inputs: _gradleInputs
			outputs: ["app/build/outputs/apk/debug/app-debug.apk"]
		}

		test: schema.#Task & {
			command: "bash"
			args: ["-c", "./gradlew --no-daemon testDebugUnitTest"]
			dependsOn: [setupSdk]
			inputs: _gradleInputs
		}

		// KVM probe + headless ATD emulator smoke suite (deviceTests
		// pipeline). API 34: the aosp-atd image line trails the newest
		// platform; 34 matches minSdk and has stable ATD images.
		gmdCheck: schema.#Task & {
			command: "bash"
			args: ["-c", "ls -l /dev/kvm && ./gradlew --no-daemon :app:atdApi34DebugAndroidTest"]
			dependsOn: [setupSdk, buildRustJni]
			inputs: _gradleInputs
		}

		// Install-in-place artifact: every merge to main rebuilds the
		// debug APK (shared checked-in debug keystore -> signature is
		// stable across builders, so `adb install -r` upgrades without an
		// uninstall) with a monotonic timestamp-derived versionCode, and
		// publishes it as the repo's "latest" release.
		//
		// Each build uses a UNIQUE tag `android-b<versionCode>-<sha>` and
		// `gh release create --latest`; the stable download URL is
		// GitHub's tag-agnostic
		//   releases/latest/download/waddle-android-debug.apk
		// which always resolves to whichever release is marked latest.
		// A fixed reused tag is deliberately NOT used: GitHub's immutable
		// releases permanently reserve a tag name once it has backed a
		// release (creating `refs/tags/<name>` afterwards 422s "Reference
		// update failed", and asset re-uploads to an existing release
		// 422 too), so delete-and-recreate on a stable tag is impossible.
		// Unique-tag-per-build sidesteps both: no tag is ever reused and
		// no published release is ever mutated. Older rolling releases
		// are pruned to a bounded window.
		publishDebugApk: schema.#Task & {
			command: "bash"
			env: {
				CI_GITHUB_TOKEN: schema.#EnvPassthrough & {name: "GITHUB_TOKEN"}
			}
			args: ["-c", #"""
				set -euo pipefail
				export GH_TOKEN="${CI_GITHUB_TOKEN:?missing GITHUB_TOKEN}"
				# Commit-timestamp minutes: monotonic across builds and immune
				# to shallow CI clones (rev-list --count would report 1).
				version_code="$(( $(git show -s --format=%ct HEAD) / 60 ))"
				./gradlew --no-daemon :app:assembleDebug \
				  -PversionCode="${version_code}" \
				  -PversionName="$(git rev-parse --short HEAD)"
				sha="$(git rev-parse --short HEAD)"
				# The asset's FILENAME (not the `file#label` display text)
				# is the path segment in releases/latest/download/<name>,
				# so upload the file under the advertised name rather than
				# gradle's app-debug.apk.
				apk="waddle-android-debug.apk"
				cp app/build/outputs/apk/debug/app-debug.apk "${apk}"
				# Unique per build (never reused -> never hits the immutable
				# tag-name lock); versionCode prefix keeps tags sortable.
				tag="android-b${version_code}-${sha}"
				gh release create "${tag}" --latest \
				  --title "Android debug ${sha}" \
				  --notes "commit ${sha} — install: adb install -r waddle-android-debug.apk (stable URL: releases/latest/download/waddle-android-debug.apk; shared debug signature, no uninstall needed)" \
				  "${apk}"
				# Prune older rolling releases beyond the newest 10, tag and
				# all (--cleanup-tag): the tags are never reused, so there
				# is no downside to deleting them and it keeps the ref list
				# bounded. Wrapped so a transient list/delete error can
				# never fail the build after a successful publish.
				{
				  gh release list --limit 100 --json tagName,createdAt \
				    --jq '[.[] | select(.tagName | startswith("android-b"))]
				          | sort_by(.createdAt) | reverse | .[10:] | .[].tagName' \
				  | while read -r old; do
				      [ -n "${old}" ] && gh release delete "${old}" --yes --cleanup-tag || true
				    done
				} || true
				"""#]
			dependsOn: [setupSdk, buildRustJni]
			// env.cue is in the input set so a change to THIS publish
			// script (not just the app) re-runs the task — otherwise
			// cuenv's incremental engine reports "no affected tasks" for
			// a script-only fix and the rolling release is never
			// rebuilt.
			inputs: list.Concat([_gradleInputs, ["env.cue"]])
			outputs: ["app/build/outputs/apk/debug/app-debug.apk"]
		}
	}
}
