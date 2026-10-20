package cuenv

import (
	"list"

	"github.com/cuenv/cuenv/schema"
	wc "github.com/waddle-social/waddle/ci/contributors"
)

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
				uses: "namespacelabs/nscloud-cache-action@1124a6f3ce44e5cf84cc22111530961f4d2a15f9" // v1.7.0
				with: path: "/home/runner/.android-sdk\n/home/runner/.gradle"
			}
		},
	]
}

// Preserve the actual render and JUnit failure when a CI screenshot check
// differs. The pipeline still fails; reviewers can inspect the evidence.
let _AndroidTestReports = schema.#Contributor & {
	id: "androidTestReports"
	tasks: [{
		id:        "android.testReports"
		label:     "Upload Android test failure reports"
		priority:  100
		condition: "on_failure"
		provider: github: {
			uses: "actions/upload-artifact@043fb46d1a93c77aae656e7c1c64a875d1fc6a0a"
			with: {
				name: "android-test-failure-reports"
				path: """
					apps/android/app/build/outputs/roborazzi/
					apps/android/app/build/reports/roborazzi/
					apps/android/app/build/test-results/
					apps/android/app/build/reports/tests/
					"""
				"if-no-files-found": "ignore"
				"retention-days":    3
			}
		}
	}]
}

// Shared Rust FFI surface: any change to the client crates retriggers the
// Android pipelines, mirroring the hand-maintained Apple workflow filters.
let _ffiInputs = [
	"../../.cargo/**",
	"../../server/.cargo/**",
	"../../server/Cargo.toml",
	"../../server/Cargo.lock",
	"../../server/vendor/**",
	"../../server/rust-toolchain.toml",
	"../../server/crates/**",
	"../../server/crates/**/.cargo/**",
	"../../scripts/build-android-rust.sh",
	"../../scripts/check-android-ffi-bindings.sh",
]

let _gradleInputs = [
	"env.cue",
	"../../scripts/android-ci-gradle.sh",
	"**/build.gradle.kts",
	"settings.gradle.kts",
	"build.gradle.kts",
	"gradle/libs.versions.toml",
	"gradle.properties",
	"gradle/**",
	"gradlew",
	"**/src",
	"**/src/**",
	"app/**",
	"core/client/build.gradle.kts",
	"core/client/consumer-rules.pro",
	"core/client/lint.xml",
	"core/client/src/**",
	"debug.keystore",
	"detekt.yml",
]

schema.#Project & {
	name: "waddle-android"

	runtime: {
		type:  "nix"
		flake: "../.."
	}

	ci: providers: ["github"]
	ci: contributors: [
		wc.#Nix,
		wc.#FlakeHubCache,
		wc.#Hestia,
		_NamespaceAndroidCache,
		_AndroidTestReports,
		wc.#CuenvRelease,
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
			provider: github: permissions: {contents: "write", "id-token": "write", packages: "none"}
			"tasks": [tasks.checkBindingsDrift, tasks.telemetryContract, tasks.ciGradleContract, tasks.lint, tasks.test, tasks.publishApk]
		}
		pullRequest: {
			when: {
				pullRequest: true
				manual:      true
			}
			provider: github: permissions: {contents: "read", "id-token": "write", packages: "none"}
			"tasks": [tasks.checkCiDrift, tasks.checkBindingsDrift, tasks.telemetryContract, tasks.ciGradleContract, tasks.lint, tasks.test, tasks.build]
		}
		// Instrumented suite on a headless GMD ATD emulator. Runs as its
		// own parallel PR job so a flaky emulator never delays unit-test
		// feedback and can be re-run independently. /dev/kvm + GMD on the
		// Namespace runner were proven by a real pull_request execution
		// (PR #1366). Caveat: a manual dispatch on an unchanged ref
		// reports green after "No affected tasks" — cuenv's affectedness
		// gate skips gmdCheck entirely, so dispatch-green on main means
		// "skipped", not "passed".
		deviceTests: {
			when: {
				pullRequest: true
				manual:      true
			}
			provider: github: permissions: {contents: "read", "id-token": "write", packages: "none"}
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
		// x86_64 into the gitignored jniLibs tree. Unoptimised dev profile:
		// PR builds and emulator suites only prove the cross-compile and
		// the bindings; nothing built from it is ever distributed.
		buildRustJni: schema.#Task & {
			command: "bash"
			args: ["-c", "../../scripts/build-android-rust.sh --debug --skip-bindings"]
			dependsOn: [setupSdk]
			inputs: _ffiInputs
			outputs: ["core/client/src/main/jniLibs/**"]
		}

		// The distributed core: `mobile` cargo profile (opt-level 3, LTO).
		// Same outputs as buildRustJni; the publish task depends on this
		// one so the shipped APK never carries an opt-level-0 library.
		buildRustJniMobile: schema.#Task & {
			command: "bash"
			args: ["-c", "../../scripts/build-android-rust.sh --skip-bindings"]
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
			args: ["sync", "ci", "--check", "-p", "."]
			inputs: ["env.cue", "../../.github/workflows/waddle-android-*.yml"]
		}

		telemetryContract: schema.#Task & {
			command: "bash"
			args: ["../../scripts/check-native-remote-telemetry.sh", "--android"]
			inputs: list.Concat([_gradleInputs, _ffiInputs, [
				"env.cue",
				"../../flake.nix",
				"../../scripts/check-native-remote-telemetry.sh",
				"../../scripts/native-telemetry-cargo-closure.py",
			]])
		}

		// Both distributions: `foss` (Obtainium, no Google code) and
		// `play` (FCM push, no background connection service). R8 also
		// runs over the Play release code (FCM library, reflective
		// serializers) with placeholder FCM values: the real ones are
		// secrets, and R8 only needs the build to configure.
		build: schema.#Task & {
			command: "bash"
			args: ["../../scripts/android-ci-gradle.sh", ":app:assembleFossDebug", ":app:assemblePlayDebug", ":app:minifyPlayReleaseWithR8", "-PWADDLE_FIREBASE_PROJECT_ID=ci-placeholder", "-PWADDLE_FIREBASE_APP_ID=1:0:android:0", "-PWADDLE_FIREBASE_API_KEY=ci-placeholder", "-PWADDLE_FIREBASE_SENDER_ID=0"]
			dependsOn: [setupSdk, buildRustJni]
			inputs: _gradleInputs
			outputs: [
				"app/build/outputs/apk/foss/debug/app-foss-debug.apk",
				"app/build/outputs/apk/play/debug/app-play-debug.apk",
			]
		}

		// JVM unit tests plus the Roborazzi golden screenshots
		// (app/src/test/screenshots), for both distributions: a visual
		// change that was not re-recorded fails here. Screenshot tests set
		// their state explicitly, so both flavours share one set of goldens.
		test: schema.#Task & {
			command: "bash"
			args: ["../../scripts/android-ci-gradle.sh", ":core:client:testDebugUnitTest", ":app:testFossDebugUnitTest", ":app:testPlayDebugUnitTest", "-Proborazzi.test.verify=true"]
			dependsOn: [setupSdk]
			inputs: _gradleInputs
		}

		// Static analysis: detekt (default rules + formatting, no
		// baselines — see detekt.yml for the justified deviations) and
		// Android lint with warnings-as-errors, mirroring the server's
		// clippy -D warnings posture. The runtime-classpath checks resolve
		// every transitive dependency: no Google code in `foss`, and no
		// analytics, crash or performance reporting in `play`.
		lint: schema.#Task & {
			command: "bash"
			args: ["../../scripts/android-ci-gradle.sh", "detekt", ":app:lintFossDebug", ":app:lintPlayDebug", ":app:verifyFossRuntimeClasspath", ":app:verifyPlayRuntimeClasspath"]
			dependsOn: [setupSdk]
			inputs: _gradleInputs
		}

		// Exercise evaluated CI entrypoints without an SDK or Gradle build.
		// Resource-policy edits must select the real lint/test/build tasks too.
		ciGradleContract: schema.#Task & {
			command: "python3"
			args: ["../../scripts/test_android_ci_gradle.py"]
			inputs: ["env.cue", "../../scripts/android-ci-gradle.sh", "../../scripts/test_android_ci_gradle.py"]
		}

		// KVM probe + headless ATD emulator suite (deviceTests pipeline).
		// API 34: the aosp-atd image line trails the newest platform; 34
		// matches minSdk and has stable ATD images. Configuration cache is
		// off for GMD runs: the emulator lifecycle interacts badly with
		// cached configuration.
		//
		// Compilation and the emulator must not overlap: GMD setup boots
		// the emulator while compile tasks run, and together they exceeded
		// the 16 GB runner, which SIGKILLed the job (exit 137) mid-compile.
		// The first Gradle run builds the app and test APKs with no
		// emulator; the second finds them up to date and only runs the
		// device. The Kotlin compiler runs in-process because a separate
		// Kotlin daemon inherits the Gradle heap and doubles it.
		gmdCheck: schema.#Task & {
			command: "bash"
			args: ["-c", #"""
				set -euo pipefail
				ls -l /dev/kvm
				bash ../../scripts/android-ci-gradle.sh --no-configuration-cache :app:assembleFossDebug :app:assembleFossDebugAndroidTest
				bash ../../scripts/android-ci-gradle.sh --no-configuration-cache :app:atdApi34FossDebugAndroidTest
				"""#]
			dependsOn: [setupSdk, buildRustJni]
			// Include this definition so GMD-only changes are selected by affectedness.
			inputs: _gradleInputs
		}

		// Install-in-place artifact: every merge to main builds the FOSS
		// `preview` APK (the Obtainium channel) — release-optimised (R8, shrinking, non-debuggable,
		// mobile-profile Rust core) but signed with the shared checked-in
		// key, so `adb install -r` upgrades over earlier rolling and debug
		// builds without an uninstall — with a monotonic timestamp-derived
		// versionCode, and publishes it as the repo's "latest" release.
		//
		// Each build uses a UNIQUE tag `android-b<versionCode>-<sha>` and
		// `gh release create --latest`; the stable download URL is
		// GitHub's tag-agnostic
		//   releases/latest/download/waddle-android.apk
		// which always resolves to whichever release is marked latest.
		// A fixed reused tag is deliberately NOT used: GitHub's immutable
		// releases permanently reserve a tag name once it has backed a
		// release (creating `refs/tags/<name>` afterwards 422s "Reference
		// update failed", and asset re-uploads to an existing release
		// 422 too), so delete-and-recreate on a stable tag is impossible.
		// Unique-tag-per-build sidesteps both: no tag is ever reused and
		// no published release is ever mutated. Older rolling releases
		// are pruned to a bounded window.
		publishApk: schema.#Task & {
			ci: github: permissions: contents: "write"
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
				bash ../../scripts/android-ci-gradle.sh :app:assembleFossPreview \
				  -PversionCode="${version_code}" \
				  -PversionName="$(git rev-parse --short HEAD)"
				sha="$(git rev-parse --short HEAD)"
				# The asset's FILENAME (not the `file#label` display text)
				# is the path segment in releases/latest/download/<name>,
				# so upload the file under the advertised name rather than
				# gradle's app-foss-preview.apk.
				apk="waddle-android.apk"
				cp app/build/outputs/apk/foss/preview/app-foss-preview.apk "${apk}"
				# Unique per build (never reused -> never hits the immutable
				# tag-name lock); versionCode prefix keeps tags sortable.
				tag="android-b${version_code}-${sha}"
				gh release create "${tag}" --latest \
				  --title "Android ${sha}" \
				  --notes "commit ${sha} — optimised preview build (R8, opt-level 3 core). Install: adb install -r waddle-android.apk (stable URL: releases/latest/download/waddle-android.apk; shared install-in-place signature, upgrades earlier rolling and debug builds without an uninstall)" \
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
			dependsOn: [setupSdk, buildRustJniMobile]
			// env.cue is in the input set so a change to THIS publish
			// script (not just the app) re-runs the task — otherwise
			// cuenv's incremental engine reports "no affected tasks" for
			// a script-only fix and the rolling release is never
			// rebuilt.
			inputs: _gradleInputs
			outputs: ["app/build/outputs/apk/foss/preview/app-foss-preview.apk"]
		}
	}
}
