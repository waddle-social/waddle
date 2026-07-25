{
  description = "Waddle Social monorepo";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    crane.url = "github:ipetkov/crane";
    rust-overlay.url = "github:oxalica/rust-overlay";
    rust-overlay.inputs.nixpkgs.follows = "nixpkgs";
  };

  outputs =
    {
      self,
      nixpkgs,
      crane,
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
          inherit waddle-server;
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
          testArgs = baseArgs // testRuntimeEnv // {
            # The Mimir-rules drift guard test (waddle-xmpp) reads the
            # rules file at <manifest>/../../../infrastructure/... — the
            # parent of the source root. Without this copy the guard
            # silently skips in every nix test lane, including the
            # dedicated xmpp unit/XEP lanes built from testArgs (#1436).
            postUnpack = ''
              mkdir -p "$sourceRoot/../infrastructure/waddle.cloud/rules/mimir"
              cp ${./infrastructure/waddle.cloud/rules/mimir/waddle-reliability.yaml} \
                "$sourceRoot/../infrastructure/waddle.cloud/rules/mimir/waddle-reliability.yaml"
            '';
          };
          serverTestArgs = testArgs // {
            postUnpack = testArgs.postUnpack + ''
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
