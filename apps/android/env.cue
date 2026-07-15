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
			provider: github: permissions: contents: "read"
			"tasks": [tasks.checkBindingsDrift, tasks.build]
		}
		pullRequest: {
			when: {
				pullRequest: true
			}
			provider: github: permissions: contents: "read"
			"tasks": [tasks.checkCiDrift, tasks.checkBindingsDrift, tasks.build]
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
	}
}
