{
  description = "Waddle Social monorepo";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    crane.url = "github:ipetkov/crane";
    cuenvSource = {
      url = "github:cuenv/cuenv/e337a3f60af6944552f391facf2ed2e25efa3bc7";
      flake = false;
    };
    rust-overlay.url = "github:oxalica/rust-overlay";
    rust-overlay.inputs.nixpkgs.follows = "nixpkgs";
  };

  outputs =
    {
      self,
      nixpkgs,
      crane,
      cuenvSource,
      rust-overlay,
    }:
    let
      systems = [
        "aarch64-darwin"
        "x86_64-darwin"
        "aarch64-linux"
        "x86_64-linux"
      ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f system);
      mkPkgs =
        system:
        import nixpkgs {
          inherit system;
          overlays = [ rust-overlay.overlays.default ];
        };
    in
    {
      packages = forAllSystems (
        system:
        let
          pkgs = mkPkgs system;
          lib = pkgs.lib;
          rustToolchain = pkgs.rust-bin.fromRustupToolchainFile ./server/rust-toolchain.toml;
          craneLib = (crane.mkLib pkgs).overrideToolchain rustToolchain;
          cuenvVersion = "0.54.0";
          # Both the Rust CLI and the Go CUE archive consume this one patched
          # source derivation. Keeping the source identity explicit prevents a
          # host or upstream bridge from masking a patched CLI build.
          cuenvPatchedSrc = pkgs.applyPatches {
            name = "cuenv-0.54.0-waddle-fail-closed-source";
            src = cuenvSource;
            patches = [ ./nix/patches/cuenv-0.54.0-fail-closed-discovery.patch ];
          };
          cuenvBridgeSrc = cuenvPatchedSrc + "/crates/cuengine";
          cuenvCueBridge = pkgs.buildGoModule {
            pname = "waddle-cuenv-cue-bridge";
            version = cuenvVersion;
            src = cuenvBridgeSrc;
            vendorHash = "sha256-p8gfl2H0lThSmqIRQZWDYoQ3antrIslpCwRCNKQ1cKs=";
            buildInputs = lib.optionals pkgs.stdenv.isDarwin [ pkgs.libiconv ];
            buildPhase = ''
              runHook preBuild
              export CGO_ENABLED=1
              mkdir -p "$out/debug" "$out/release"
              go_sources=$(find . -maxdepth 1 -name '*.go' ! -name '*_test.go' -print | sort)
              go build -buildmode=c-archive -o "$out/debug/libcue_bridge.a" $go_sources
              cp libcue_bridge.h "$out/debug/"
              go build -buildmode=c-archive -o "$out/release/libcue_bridge.a" $go_sources
              cp libcue_bridge.h "$out/release/"
              runHook postBuild
            '';
            installPhase = ''
              runHook preInstall
              runHook postInstall
            '';
            passthru.source = cuenvPatchedSrc;
          };
          cuenvBaseArgs = {
            pname = "cuenv";
            version = cuenvVersion;
            src = cuenvPatchedSrc;
            strictDeps = true;
            cargoExtraArgs = "--locked --package cuenv";
            nativeBuildInputs = [
              pkgs.go
              pkgs.pkg-config
            ];
            buildInputs = lib.optionals pkgs.stdenv.isDarwin [ pkgs.libiconv ];
            CUE_BRIDGE_PATH = cuenvCueBridge;
          };
          cuenvCargoArtifacts = craneLib.buildDepsOnly (
            cuenvBaseArgs
            // {
              doCheck = false;
            }
          );
          cuenv = craneLib.buildPackage (
            cuenvBaseArgs
            // {
              cargoArtifacts = cuenvCargoArtifacts;
              doCheck = false;
              passthru = {
                source = cuenvPatchedSrc;
                cueBridge = cuenvCueBridge;
              };
            }
          );
          cuenvFixtureModule = pkgs.writeText "waddle-cuenv-fixture-module.cue" ''
            module: "example.com/waddle-cuenv-fixture"
            language: {
              version: "v0.9.0"
            }
          '';
          cuenvFixtureHealthy = pkgs.writeText "waddle-cuenv-fixture-healthy.cue" ''
            package cuenv

            name: "healthy"
            tasks: {
              check: {
                command: "true"
                inputs: ["env.cue"]
              }
            }
          '';
          cuenvFixtureBrokenSelected = pkgs.writeText "waddle-cuenv-fixture-broken-selected.cue" ''
            package cuenv

            broken: {
          '';
          cuenvFixtureNoPackage = pkgs.writeText "waddle-cuenv-fixture-no-package.cue" ''
            unknown: true
          '';
          cuenvFixtureOpenList = pkgs.writeText "waddle-cuenv-fixture-open-list.cue" ''
            package cuenv

            open: [...string]
          '';
          cuenvFixtureNestedIncomplete = pkgs.writeText "waddle-cuenv-fixture-nested-incomplete.cue" ''
            package cuenv

            outer: {
              ready: "yes"
              pending: string
            }
          '';
          cuenvFixtureBrokenOther = pkgs.writeText "waddle-cuenv-fixture-broken-other.cue" ''
            package other

            broken: {
          '';
          cuenvSourceCoupling =
            pkgs.runCommand "waddle-cuenv-source-coupling"
              {
                nativeBuildInputs = [
                  cuenv
                  cuenvCueBridge
                ];
                expectedSource = cuenvPatchedSrc;
                cliSource = cuenv.passthru.source;
                bridgeSource = cuenvCueBridge.passthru.source;
              }
              ''
                test "$expectedSource" = "$cliSource"
                test "$expectedSource" = "$bridgeSource"
                grep -F 'Selected CUE instances failed to evaluate' "$expectedSource/crates/cuengine/bridge.go"
                {
                  printf '%s\n' "source=$expectedSource"
                  printf '%s\n' "cli_source=$cliSource"
                  printf '%s\n' "bridge_source=$bridgeSource"
                } > "$out"
              '';
          cuenvStrictDiscovery =
            pkgs.runCommand "waddle-cuenv-strict-discovery"
              {
                nativeBuildInputs = [
                  cuenv
                  pkgs.git
                  pkgs.jq
                ];
              }
              ''
                            export XDG_CONFIG_HOME="$TMPDIR/xdg-config"
                            export XDG_CACHE_HOME="$TMPDIR/xdg-cache"
                            mkdir -p "$XDG_CONFIG_HOME" "$XDG_CACHE_HOME" "$out"

                            prepare_fixture() {
                              fixture="$1"
                  mkdir -p "$fixture/cue.mod"
                  install -Dm444 ${cuenvFixtureModule} "$fixture/cue.mod/module.cue"
                  install -Dm444 ${cuenvFixtureHealthy} "$fixture/env.cue"
                  git -C "$fixture" init -q
                              git -C "$fixture" config user.name "Waddle cuenv proof"
                              git -C "$fixture" config user.email "cuenv-proof@example.invalid"
                            }

                            expect_failure() {
                              name="$1"
                              expected_path="$2"
                              fixture="$3"
                              shift 3
                              if (cd "$fixture" && "$@") > "$out/$name.log" 2>&1; then
                                echo "expected $name to fail" >&2
                                sed -n '1,240p' "$out/$name.log" >&2
                                exit 1
                              fi
                              grep -F "$expected_path" "$out/$name.log"
                            }

                            bad_selected="$TMPDIR/bad-selected-fixture"
                            prepare_fixture "$bad_selected"
                            mkdir -p "$bad_selected/bad-selected"
                            install -Dm444 ${cuenvFixtureBrokenSelected} "$bad_selected/bad-selected/env.cue"
                            expect_failure bad-selected-info bad-selected "$bad_selected" ${cuenv}/bin/cuenv info --json
                            expect_failure bad-selected-sync-all bad-selected "$bad_selected" ${cuenv}/bin/cuenv sync -A
                            expect_failure bad-selected-sync-ci-check-all bad-selected "$bad_selected" ${cuenv}/bin/cuenv sync ci --check -A

                            no_package="$TMPDIR/no-package-fixture"
                    prepare_fixture "$no_package"
                    mkdir -p "$no_package/unknown-package"
                    install -Dm444 ${cuenvFixtureNoPackage} "$no_package/unknown-package/env.cue"
                    expect_failure no-package-sync-all unknown-package "$no_package" ${cuenv}/bin/cuenv sync -A

                            open_list="$TMPDIR/open-list-fixture"
                            prepare_fixture "$open_list"
                            mkdir -p "$open_list/open-list"
                            install -Dm444 ${cuenvFixtureOpenList} "$open_list/open-list/env.cue"
                            expect_failure open-list-info open-list "$open_list" ${cuenv}/bin/cuenv info --json

                            nested_incomplete="$TMPDIR/nested-incomplete-fixture"
                            prepare_fixture "$nested_incomplete"
                            mkdir -p "$nested_incomplete/nested-incomplete"
                            install -Dm444 ${cuenvFixtureNestedIncomplete} "$nested_incomplete/nested-incomplete/env.cue"
                            expect_failure nested-incomplete-info nested-incomplete "$nested_incomplete" ${cuenv}/bin/cuenv info --json

                            other_package="$TMPDIR/other-package-fixture"
                prepare_fixture "$other_package"
                mkdir -p "$other_package/malformed-other"
                install -Dm444 ${cuenvFixtureBrokenOther} "$other_package/malformed-other/env.cue"
                if ! (cd "$other_package" && ${cuenv}/bin/cuenv info --json) \
                              > "$out/malformed-other-info.json" \
                              2> "$out/malformed-other-info.stderr"; then
                              echo "expected malformed positively-other package to remain isolated" >&2
                              sed -n '1,240p' "$out/malformed-other-info.stderr" >&2
                              exit 1
                            fi
                            jq -e '
                              .project_count == 1
                              and .projects == [{"name": "healthy", "path": "."}]
                            ' "$out/malformed-other-info.json" > /dev/null
              '';
          cuenvWaddleDiscovery =
            pkgs.runCommand "waddle-cuenv-waddle-discovery"
              {
                nativeBuildInputs = [
                  cuenv
                  pkgs.jq
                ];
                waddleSource = self;
              }
              ''
                export HOME="$TMPDIR/home"
                mkdir -p "$HOME" "$out"
                cd "$waddleSource"
                ${cuenv}/bin/cuenv info --json > "$out/info.json"
                jq -e '
                  .project_count == 6
                  and .projects == [
                    {"name": "waddle-android", "path": "apps/android"},
                    {"name": "waddle-chat", "path": "chat"},
                    {"name": "waddle-cloud", "path": "infrastructure/waddle.cloud"},
                    {"name": "waddle-colony", "path": "colony"},
                    {"name": "waddle-server", "path": "server"},
                    {"name": "waddle-website", "path": "website"}
                  ]
                ' "$out/info.json" > /dev/null
              '';
          serverPackageSrc = lib.fileset.toSource {
            root = ./server;
            fileset =
              let
                testFiles = lib.fileset.unions [
                  ./server/crates/waddle-server/tests
                  ./server/crates/waddle-xmpp/tests
                ];
              in
              lib.fileset.unions [
                ./server/Cargo.toml
                ./server/Cargo.lock
                (lib.fileset.difference ./server/crates testFiles)
                ./server/extensions
                ./server/wit
              ];
          };
          serverCheckSrc = lib.fileset.toSource {
            root = ./server;
            fileset = lib.fileset.unions [
              ./server/Cargo.toml
              ./server/Cargo.lock
              ./server/capabilities.toml
              ./server/crates
              ./server/extensions
              ./server/wit
            ];
          };
          baseArgs = {
            pname = "waddle-server";
            version = "0.1.0";
            src = serverPackageSrc;
            strictDeps = true;
            cargoExtraArgs = "--locked --package waddle-server --bin waddle-server --features clustering";
            nativeBuildInputs = [
              pkgs.pkg-config
              pkgs.protobuf
            ];
            buildInputs = [
              pkgs.openssl
              pkgs.sqlite
            ];
          };
          checkBaseArgs = baseArgs // {
            src = serverCheckSrc;
            nativeBuildInputs = baseArgs.nativeBuildInputs ++ [
              pkgs.go
            ];
            preBuild = ''
              export HOME="$TMPDIR"
              export GOCACHE="$TMPDIR/go-cache"
              export GOMODCACHE="$TMPDIR/go-mod-cache"
            '';
          };
          cargoArtifacts = craneLib.buildDepsOnly (
            baseArgs
            // {
              doCheck = false;
              cargoCheckExtraArgs = "";
            }
          );
          workspaceArtifacts = craneLib.buildDepsOnly (
            checkBaseArgs
            // {
              cargoExtraArgs = "--locked --workspace";
              cargoCheckExtraArgs = "--all-targets";
              cargoBuildExtraArgs = "--all-targets";
              cargoTestExtraArgs = "--all-targets --no-run";
            }
          );
          workspaceAllFeaturesArtifacts = craneLib.buildDepsOnly (
            checkBaseArgs
            // {
              cargoExtraArgs = "--locked --workspace --all-features";
              cargoCheckExtraArgs = "--all-targets";
              cargoBuildExtraArgs = "--all-targets";
              cargoTestExtraArgs = "--all-targets --no-run";
            }
          );
          xmppArtifacts = craneLib.buildDepsOnly (
            checkBaseArgs
            // {
              cargoExtraArgs = "--locked --package waddle-xmpp";
              cargoCheckExtraArgs = "--all-targets";
              cargoBuildExtraArgs = "--all-targets";
              cargoTestExtraArgs = "--all-targets --no-run";
            }
          );
          serverTestArtifacts = craneLib.buildDepsOnly (
            checkBaseArgs
            // {
              cargoExtraArgs = "--locked --package waddle-server";
              cargoCheckExtraArgs = "--all-targets";
              cargoBuildExtraArgs = "--all-targets";
              cargoTestExtraArgs = "--all-targets --no-run";
            }
          );
          waddle-server = craneLib.buildPackage (
            baseArgs
            // {
              inherit cargoArtifacts;
              doCheck = false;
            }
          );
          image = pkgs.dockerTools.streamLayeredImage {
            name = "ghcr.io/waddle-social/waddle";
            tag = "nix";
            contents = [
              waddle-server
              pkgs.cacert
              pkgs.iana-etc
            ];
            fakeRootCommands = ''
              ${pkgs.dockerTools.shadowSetup}
              groupadd -r waddle
              useradd -r -g waddle -d /var/lib/waddle -s /usr/sbin/nologin waddle
              mkdir -p /app /var/lib/waddle
              chown waddle:waddle /var/lib/waddle
            '';
            enableFakechroot = true;
            config = {
              Entrypoint = [ "${lib.getExe waddle-server}" ];
              WorkingDir = "/app";
              User = "waddle:waddle";
              ExposedPorts = {
                "3000/tcp" = { };
                "5269/tcp" = { };
              };
            };
          };
        in
        {
          inherit waddle-server cuenv;
          cuenv-cue-bridge = cuenvCueBridge;
          cuenv-source-coupling = cuenvSourceCoupling;
          cuenv-strict-discovery = cuenvStrictDiscovery;
          cuenv-waddle-discovery = cuenvWaddleDiscovery;
          waddle-server-deps = cargoArtifacts;
          waddle-server-workspace-deps = workspaceArtifacts;
          waddle-server-workspace-all-features-deps = workspaceAllFeaturesArtifacts;
          waddle-server-xmpp-deps = xmppArtifacts;
          waddle-server-test-deps = serverTestArtifacts;
          default = waddle-server;
        }
        // lib.optionalAttrs pkgs.stdenv.isLinux {
          waddle-server-image-stream = image;
        }
      );

      checks = forAllSystems (
        system:
        let
          pkgs = mkPkgs system;
          lib = pkgs.lib;
          rustToolchain = pkgs.rust-bin.fromRustupToolchainFile ./server/rust-toolchain.toml;
          craneLib = (crane.mkLib pkgs).overrideToolchain rustToolchain;
          serverCheckSrc = lib.fileset.toSource {
            root = ./server;
            fileset = lib.fileset.unions [
              ./server/Cargo.toml
              ./server/Cargo.lock
              ./server/.config/nextest.toml
              ./server/capabilities.toml
              ./server/crates
              ./server/extensions
              ./server/wit
            ];
          };
          baseArgs = {
            pname = "waddle-server";
            version = "0.1.0";
            src = serverCheckSrc;
            strictDeps = true;
            nativeBuildInputs = [
              pkgs.pkg-config
              pkgs.protobuf
              pkgs.go
            ];
            buildInputs = [
              pkgs.openssl
              pkgs.sqlite
            ];
            preBuild = ''
              export HOME="$TMPDIR"
              export GOCACHE="$TMPDIR/go-cache"
              export GOMODCACHE="$TMPDIR/go-mod-cache"
            '';
          };
          testRuntimeEnv = {
            WADDLE_CERTS_EPHEMERAL = "true";
            WADDLE_TEST_FIXED_ACCOUNT_ENABLED = "true";
            WADDLE_TEST_FIXED_ACCOUNT_PASSWORD = "cuenv-test-password";
            WADDLE_UPLOAD_DIR = "./uploads";
            RUST_BACKTRACE = "1";
          };
          testArgs = baseArgs // testRuntimeEnv;
          serverTestArgs = testArgs // {
            postUnpack = ''
              cp -R ${./server/charts} "$sourceRoot/charts"
            '';
          };
          serverPostgresTestArgs = serverTestArgs // {
            nativeBuildInputs = serverTestArgs.nativeBuildInputs ++ [
              pkgs.postgresql
            ];
            preCheck = ''
              export PGDATA="$TMPDIR/postgres-data"
              export PGHOST="$TMPDIR/postgres-socket"
              export PGPORT=55432
              mkdir -p "$PGHOST"

              cleanup_waddle_test_postgres() {
                if [ -n "''${PGDATA:-}" ] && [ -d "$PGDATA" ]; then
                  pg_ctl -D "$PGDATA" -m fast -w stop || true
                fi
              }
              trap cleanup_waddle_test_postgres EXIT

              initdb -D "$PGDATA" -U waddle_test -A trust --no-locale --encoding=UTF8
              pg_ctl -D "$PGDATA" -o "-k $PGHOST -p $PGPORT -c listen_addresses=" -w start
              createdb -h "$PGHOST" -p "$PGPORT" -U waddle_test waddle_test
              export WADDLE_TEST_POSTGRES_URL="postgresql:///waddle_test?user=waddle_test&host=$PGHOST&port=$PGPORT"
            '';
          };
          # Lint/test derivations build with the `ci-test` cargo profile
          # (no LTO, codegen-units = 16, opt-level = 1) instead of the
          # production `release` profile (fat LTO, codegen-units = 1):
          # tests don't need production codegen, and LTO linking of the
          # ~200 test binaries dominated check wall-clock. Shipped builds
          # (nixBuildCi, the waddle-server package) keep release/ci.
          workspaceAllFeaturesArtifacts = craneLib.buildDepsOnly (
            baseArgs
            // {
              CARGO_PROFILE = "ci-test";
              cargoExtraArgs = "--locked --workspace --all-features";
              cargoCheckExtraArgs = "--all-targets";
              cargoBuildExtraArgs = "--all-targets";
              cargoTestExtraArgs = "--all-targets --no-run";
            }
          );
          xmppArtifacts = craneLib.buildDepsOnly (
            baseArgs
            // {
              CARGO_PROFILE = "ci-test";
              cargoExtraArgs = "--locked --package waddle-xmpp";
              cargoCheckExtraArgs = "--all-targets";
              cargoBuildExtraArgs = "--all-targets";
              cargoTestExtraArgs = "--all-targets --no-run";
            }
          );
          serverTestArtifacts = craneLib.buildDepsOnly (
            baseArgs
            // {
              CARGO_PROFILE = "ci-test";
              cargoExtraArgs = "--locked --package waddle-server";
              cargoCheckExtraArgs = "--all-targets";
              cargoBuildExtraArgs = "--all-targets";
              cargoTestExtraArgs = "--all-targets --no-run";
            }
          );
          ciServerArtifacts = craneLib.buildDepsOnly (
            baseArgs
            // {
              CARGO_PROFILE = "ci";
              cargoExtraArgs = "--locked --package waddle-server --bin waddle-server --features clustering";
              cargoCheckExtraArgs = "";
              cargoBuildExtraArgs = "";
              cargoTestExtraArgs = "";
            }
          );
          extensionWasmArtifacts = craneLib.buildDepsOnly (
            baseArgs
            // {
              CARGO_BUILD_TARGET = "wasm32-wasip2";
              cargoExtraArgs = "--locked --package ai-chatbot --package decision-polls --package github --package link-board --package stargate-quotes";
              cargoCheckExtraArgs = "";
              cargoBuildExtraArgs = "";
              cargoTestExtraArgs = "";
              doCheck = false;
            }
          );
        in
        {
          waddle-server-fmt = craneLib.cargoFmt {
            pname = "waddle-server-fmt";
            version = "0.1.0";
            src = serverCheckSrc;
            cargoExtraArgs = "--all";
          };
          waddle-server-clippy = craneLib.cargoClippy (
            baseArgs
            // {
              CARGO_PROFILE = "ci-test";
              cargoArtifacts = workspaceAllFeaturesArtifacts;
              cargoExtraArgs = "--locked --workspace --all-features";
              cargoClippyExtraArgs = "--all-targets -- -D warnings";
            }
          );
          waddle-server-test = craneLib.cargoNextest (
            serverPostgresTestArgs
            // {
              CARGO_PROFILE = "ci-test";
              cargoArtifacts = workspaceAllFeaturesArtifacts;
              cargoExtraArgs = "--locked --workspace --all-features";
              cargoNextestExtraArgs = "--profile ci --lib --tests";
            }
          );
          waddle-server-doctest = craneLib.cargoTest (
            testArgs
            // {
              pname = "waddle-server-doctest";
              CARGO_PROFILE = "ci-test";
              cargoArtifacts = workspaceAllFeaturesArtifacts;
              cargoExtraArgs = "--locked --workspace --all-features";
              cargoTestExtraArgs = "--doc";
            }
          );
          waddle-server-ci-build = craneLib.cargoBuild (
            baseArgs
            // {
              pname = "waddle-server-ci-build";
              CARGO_PROFILE = "ci";
              cargoArtifacts = ciServerArtifacts;
              cargoExtraArgs = "--locked --package waddle-server --bin waddle-server --features clustering";
            }
          );
          waddle-server-extension-modules = craneLib.cargoBuild (
            baseArgs
            // {
              pname = "waddle-server-extension-modules";
              CARGO_BUILD_TARGET = "wasm32-wasip2";
              cargoArtifacts = extensionWasmArtifacts;
              cargoExtraArgs = "--locked --package ai-chatbot --package decision-polls --package github --package link-board --package stargate-quotes";
            }
          );
          waddle-server-xmpp-unit-tests = craneLib.cargoNextest (
            testArgs
            // {
              pname = "waddle-server-xmpp-unit-tests";
              CARGO_PROFILE = "ci-test";
              cargoArtifacts = xmppArtifacts;
              cargoExtraArgs = "--locked --package waddle-xmpp --features test-utils";
              cargoNextestExtraArgs = "--profile ci --lib";
            }
          );
          waddle-server-xmpp-server-tests = craneLib.cargoNextest (
            serverPostgresTestArgs
            // {
              pname = "waddle-server-xmpp-server-tests";
              CARGO_PROFILE = "ci-test";
              cargoArtifacts = serverTestArtifacts;
              cargoExtraArgs = "--locked --package waddle-server";
              cargoNextestExtraArgs = "--profile ci --lib --tests";
            }
          );
          waddle-server-xmpp-cue-e2e = craneLib.cargoNextest (
            serverTestArgs
            // {
              pname = "waddle-server-xmpp-cue-e2e";
              CARGO_PROFILE = "ci-test";
              cargoArtifacts = serverTestArtifacts;
              cargoExtraArgs = "--locked --package waddle-server";
              cargoNextestExtraArgs = "--profile ci --test xmpp_e2e_cue";
            }
          );
          waddle-server-xmpp-xep-integration = craneLib.cargoNextest (
            testArgs
            // {
              pname = "waddle-server-xmpp-xep-integration";
              CARGO_PROFILE = "ci-test";
              cargoArtifacts = xmppArtifacts;
              cargoExtraArgs = "--locked --package waddle-xmpp --features test-utils";
              cargoNextestExtraArgs = "--profile ci --tests";
            }
          );
        }
      );

      devShells = forAllSystems (
        system:
        let
          pkgs = mkPkgs system;
          rustToolchain = pkgs.rust-bin.fromRustupToolchainFile ./server/rust-toolchain.toml;
        in
        {
          default = pkgs.mkShell {
            packages = [
              rustToolchain
              pkgs.bun
              pkgs.nodejs_22
              pkgs.go
              pkgs.cue
              pkgs.kubectl
              pkgs.kubernetes-helm
              pkgs.just
              pkgs.jujutsu
              pkgs.cargo-chef
              pkgs.cargo-nextest
              pkgs.wasm-pack
              pkgs.teleport
              pkgs.openssl
              pkgs.pkg-config
              pkgs.protobuf
              pkgs.oras
              pkgs.fluxcd
              pkgs.yq-go
              # Alerts-as-code (#1324): rule lint on PRs + ruler sync
              # on main push need mimirtool and lokitool.
              pkgs.mimir
              pkgs.grafana-loki
              # Android app (apps/android): JDK for sdkmanager/Gradle,
              # cargo-ndk for the jniLibs cross-build, gh for the release
              # APK upload task. The Android SDK itself is provisioned by
              # scripts/setup-android-sdk.sh, not nix.
              pkgs.temurin-bin-21
              pkgs.cargo-ndk
              pkgs.gh
            ];

            env.JAVA_HOME = "${pkgs.temurin-bin-21}";

            shellHook = ''
              echo "waddle dev shell"
              echo "  rust: $(rustc --version)"
              echo "  bun:  $(bun --version)"
              echo "  node: $(node --version)"
            '';
          };
        }
      );
    };
}
