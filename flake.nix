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
      mkTestRustcWrapper =
        pkgs:
        pkgs.writeShellScript "waddle-test-archive-rustc" ''
          # Keep Nix's linker wrapper, including its runtime library paths.
          export PATH="${pkgs.lib.makeBinPath [ pkgs.mold ]}:$PATH"
          exec "$@" -C linker-features=-lld -C link-arg=-fuse-ld=mold
        '';
    in
    {
      packages = forAllSystems (
        system:
        let
          pkgs = mkPkgs system;
          lib = pkgs.lib;
          rustToolchain = pkgs.rust-bin.fromRustupToolchainFile ./server/rust-toolchain.toml;
          craneLib = (crane.mkLib pkgs).overrideToolchain rustToolchain;
          testArchiveRustc = mkTestRustcWrapper pkgs;
          testBuilderMemoryGuard = ''
            # MemTotal can exceed a container's cgroup limit. Enforce
            # both when cgroup v2 exposes its memory limit. Allow for
            # kernel reservations on a nominal 64 GB runner.
            required_memory_kib=$((56 * 1024 * 1024))
            available_memory_kib=$(awk '/^MemTotal:/ { print $2 }' /proc/meminfo)
            if [ -r /sys/fs/cgroup/memory.max ]; then
              cgroup_memory_bytes=$(cat /sys/fs/cgroup/memory.max)
              if [ "$cgroup_memory_bytes" != max ]; then
                cgroup_memory_kib=$((cgroup_memory_bytes / 1024))
                if [ "$cgroup_memory_kib" -lt "$available_memory_kib" ]; then
                  available_memory_kib=$cgroup_memory_kib
                fi
              fi
            fi
            if [ "$available_memory_kib" -lt "$required_memory_kib" ]; then
              echo "waddle-server-test-archive requires a 64 GB builder; use waddle-server-test on smaller machines" >&2
              exit 1
            fi
          '';
          # Opt in to a 64 GB builder; the ordinary check keeps one Cargo job.
          # Build portable archives once. Four isolated Nix sandboxes run
          # them on disjoint eight-CPU sets on the same 32-core builder.
          serverTestArchive = self.checks.${system}.waddle-server-test.overrideAttrs (old: {
            pname = "waddle-server-test-archive";
            outputs = [
              "out"
              "shard1"
              "shard2"
              "shard3"
              "shard4"
            ];
            CARGO_BUILD_JOBS = "4";
            doInstallCargoArtifacts = false;
            # Metadata lives in out; it contains no runtime libraries. Avoid
            # stdenv adding out/lib to every shard's executable RPATH.
            NIX_NO_SELF_RPATH = "1";
            # Compilation and test listing do not require a live database.
            preCheck = "";
            nativeBuildInputs = lib.filter (input: input != pkgs.postgresql_17) old.nativeBuildInputs;
            checkPhase = ''
              ${testBuilderMemoryGuard}
              runHook preCheck
              ${lib.optionalString pkgs.stdenv.hostPlatform.isLinux ''
                # Cargo fingerprints this wrapper for workspace members only;
                # cached third-party dependencies retain their existing flags.
                export RUSTC_WORKSPACE_WRAPPER=${testArchiveRustc}
                echo "WADDLE_CI_METRIC phase=compile linker=mold version=${pkgs.mold.version}"
              ''}
              mkdir -p "$out/ci-performance"
              # Keep third-party dependencies and production profiles unchanged.
              # Cargo tracks these overrides explicitly for the two largest
              # workspace crates; every build/list/archive call shares them.
              nextest_args=(--cargo-profile "$CARGO_PROFILE" --locked --workspace --all-features --profile ci --lib --tests
                --config profile.ci-test.package.waddle-server.opt-level=0
                --config profile.ci-test.package.waddle-xmpp.opt-level=0)
              echo "WADDLE_CI_METRIC phase=compile waddle_server_opt_level=0 waddle_xmpp_opt_level=0 dependency_profile_unchanged=true"
              ${pkgs.time}/bin/time -f 'WADDLE_CI_METRIC phase=compile elapsed_seconds=%e user_seconds=%U system_seconds=%S cpu=%P max_process_rss_kib=%M exit_code=%x' \
                  cargo nextest run "''${nextest_args[@]}" --no-run --timings
                # This is the visible cgroup's lifetime peak, which can
                # include dependency preparation; it is not a phase RSS.
                if [ -r /sys/fs/cgroup/memory.peak ]; then
                  echo "WADDLE_CI_METRIC phase=compile cgroup_lifetime_peak_bytes=$(cat /sys/fs/cgroup/memory.peak)"
                fi
                if [ -r /sys/fs/cgroup/memory.events ]; then
                  while read -r event count; do
                    echo "WADDLE_CI_METRIC phase=compile cgroup_lifetime_memory_event=$event count=$count"
                  done < /sys/fs/cgroup/memory.events
                fi
              cp "''${CARGO_TARGET_DIR:-target}/cargo-timings/cargo-timing.html" "$out/ci-performance/cargo-timing.html"
              cargo nextest list "''${nextest_args[@]}" --message-format json > "$out/test-inventory.json"
              ${pkgs.python3}/bin/python3 ${./server/scripts/plan_nextest_binary_shards.py} \
                "$out/test-inventory.json" "$out" --count 4 \
                --shared-binary waddle-server --shared-binary waddle-server::clustering_cluster_e2e
              archive_outputs=("$shard1" "$shard2" "$shard3" "$shard4")
              coverage_inventories=()
              set -o pipefail
              for partition in 1 2 3 4; do
                archive_output="''${archive_outputs[$((partition - 1))]}"
                mkdir -p "$archive_output"
                filter=$(cat "$out/partition-$partition.filter")
                cargo nextest list "''${nextest_args[@]}" -E "$filter" \
                  --message-format json > "$out/partition-$partition.json"
                cp "$out/partition-$partition.whole.filter" "$archive_output/whole.filter"
                cp "$out/shared.filter" "$archive_output/shared.filter"
                cargo nextest list "''${nextest_args[@]}" -E "$(cat "$archive_output/whole.filter")" \
                  --message-format json > "$out/whole-$partition.json"
                cargo nextest list "''${nextest_args[@]}" -E "$(cat "$archive_output/shared.filter")" \
                  --partition "hash:$partition/4" --message-format json > "$out/shared-$partition.json"
                # Comparison metadata contains compiler/vendor paths, not
                # runtime roots. Preserve its full contents compressed; ELF
                # runtime dependencies remain explicit below.
                ${pkgs.gzip}/bin/gzip -n -c "$out/whole-$partition.json" > "$archive_output/whole-inventory.json.gz"
                ${pkgs.gzip}/bin/gzip -n -c "$out/shared-$partition.json" > "$archive_output/shared-inventory.json.gz"
                coverage_inventories+=("$out/whole-$partition.json" "$out/shared-$partition.json")
              done
              ${pkgs.python3}/bin/python3 ${./server/scripts/check_nextest_shards.py} \
                "$out/test-inventory.json" "''${coverage_inventories[@]}" --count 8 \
                --shared-binary waddle-server --shared-binary waddle-server::clustering_cluster_e2e \
                > "$out/partition-coverage.json"
              # Cargo inventory creation has finished before packaging starts.
              # Each job writes only its own output. Cargo's build lock still
              # serializes its brief freshness check; compression, ELF checks
              # and content hashing can use the otherwise idle builder cores.
              package_archive() (
                set -euo pipefail
                partition="$1"
                archive_output="''${archive_outputs[$((partition - 1))]}"
                filter=$(cat "$out/partition-$partition.filter")
                ${pkgs.time}/bin/time -f "WADDLE_CI_METRIC phase=archive shard=$partition elapsed_seconds=%e user_seconds=%U system_seconds=%S cpu=%P max_process_rss_kib=%M exit_code=%x" \
                  cargo nextest archive "''${nextest_args[@]}" -E "$filter" \
                    --archive-format tar-zst --archive-file "$archive_output/archive.tar.zst"
                # Mirror nextest's native binary filtering when retaining Nix
                # runtime roots: selected test binaries and their packages'
                # executable helpers. Compressed ELFs hide these references.
                ${pkgs.jq}/bin/jq -r --arg target "''${CARGO_TARGET_DIR:-target}" '
                  . as $inventory |
                  [.["rust-suites"][] | select(.status == "listed")] as $suites |
                  ($suites[] | .["binary-path"]),
                  ($suites | map(.["package-id"]) | unique[] as $package |
                    $inventory["rust-build-meta"]["non-test-binaries"][$package][]? |
                    select(.kind == "bin-exe") | $target + "/" + .path)
                ' "$out/partition-$partition.json" | sort -u | while IFS= read -r binary; do
                  # Read headers and linker identity without loading whole ELFs.
                  binary_metadata=$(${pkgs.binutils}/bin/readelf --wide --program-headers --dynamic --string-dump=.comment "$binary")
                  ${lib.optionalString pkgs.stdenv.hostPlatform.isLinux ''
                    if ! grep -Eq '] +mold ${lib.escapeRegex pkgs.mold.version}( |$)' <<< "$binary_metadata"; then
                      echo "Expected mold ${pkgs.mold.version} in archived binary: $binary" >&2
                      exit 1
                    fi
                    echo "WADDLE_CI_METRIC phase=linker_verified linker=mold shard=$partition binary=$binary" >&2
                  ''}
                  printf '%s\n' "$binary_metadata" |
                    awk '/Requesting program interpreter:/ || /\((RPATH|RUNPATH)\)/ {
                      sub(/\][[:space:]]*$/, "")
                      print
                    }'
                done | grep -Eo '/nix/store/[a-z0-9]{32}-[^/:[:space:]]+' | sort -u > "$archive_output/runtime-references"
                # Bind the exact bytes consumed by each worker, regardless of
                # whether they arrive through a binary cache or raw artifact.
                content_hash_started=$SECONDS
                (cd "$archive_output" && ${pkgs.coreutils}/bin/sha256sum \
                  archive.tar.zst whole.filter shared.filter \
                  whole-inventory.json.gz shared-inventory.json.gz runtime-references) \
                  > "$archive_output/archive-content-checksums"
                echo "WADDLE_CI_METRIC phase=archive_content_hash shard=$partition elapsed_seconds=$((SECONDS - content_hash_started))"
                wc -c "$archive_output/archive.tar.zst"
              )
              package_started=$SECONDS
              package_pids=()
              for partition in 1 2 3 4; do
                package_archive "$partition" &
                package_pids+=("$!")
              done
              package_status=0
              for package_pid in "''${package_pids[@]}"; do
                if wait "$package_pid"; then
                  :
                else
                  package_status=1
                fi
              done
              if [[ "$package_status" -ne 0 ]]; then
                echo "At least one test archive failed to package or verify" >&2
                exit "$package_status"
              fi
              echo "WADDLE_CI_METRIC phase=archive_all elapsed_seconds=$((SECONDS - package_started)) concurrency=4"
              runHook postCheck
            '';
          });
          mkServerTestShard =
            partition:
            let
              testCheck = self.checks.${system}.waddle-server-test;
              archiveOutput = serverTestArchive.${"shard${toString partition}"};
            in
            pkgs.stdenvNoCC.mkDerivation {
              # Execute compiled archives without inheriting Crane compiler,
              # vendoring hooks, or development outputs. Retain test fixtures
              # and the exact database/environment setup from the full lane.
              inherit (testCheck)
                version
                src
                strictDeps
                postUnpack
                preBuild
                preCheck
                SSL_CERT_FILE
                WADDLE_CERTS_EPHEMERAL
                WADDLE_TEST_FIXED_ACCOUNT_ENABLED
                WADDLE_TEST_FIXED_ACCOUNT_PASSWORD
                WADDLE_UPLOAD_DIR
                RUST_BACKTRACE
                NEXTEST_SHOW_PROGRESS
                ;
              pname = "waddle-server-test-shard-${toString partition}";
              nativeBuildInputs = [ pkgs.postgresql_17.out ];
              dontConfigure = true;
              doCheck = true;
              buildPhase = ''
                runHook preBuild
                runHook postBuild
              '';
              installPhase = ''mkdir -p "$out"'';
              checkPhase = ''
                ${pkgs.python3}/bin/python3 ${./server/scripts/pin-nextest-shard.py} \
                  --parent-pid "$$" --partition ${toString partition} --count 4 --cores 8
                runHook preCheck
                mkdir -p "$out"
                extracted="$TMPDIR/nextest-archive"
                mkdir -p "$extracted"
                whole_filter=$(cat ${archiveOutput}/whole.filter)
                shared_filter=$(cat ${archiveOutput}/shared.filter)
                # Nextest sets runtime manifest/binary paths after remapping.
                # Nix uses different temporary source roots on each worker.
                ${pkgs.cargo-nextest}/bin/cargo-nextest nextest list --profile ci --archive-file ${archiveOutput}/archive.tar.zst \
                  --extract-to "$extracted" --workspace-remap "$PWD" \
                  -E "$whole_filter" --message-format json > "$out/whole-inventory.json"
                ${pkgs.python3}/bin/python3 ${./server/scripts/check_nextest_shards.py} --compare-partition \
                  ${archiveOutput}/whole-inventory.json.gz "$out/whole-inventory.json" \
                  > "$out/whole-coverage.json"
                # Both phases reuse one extraction. The two slow binaries are
                # shared across workers, with disjoint per-test partitions.
                reused_args=(--profile ci
                  --cargo-metadata "$extracted/target/nextest/cargo-metadata.json"
                  --binaries-metadata "$extracted/target/nextest/binaries-metadata.json"
                  --target-dir-remap "$extracted/target" --build-dir-remap "$extracted/target"
                  --workspace-remap "$PWD")
                ${pkgs.cargo-nextest}/bin/cargo-nextest nextest list "''${reused_args[@]}" -E "$shared_filter" \
                  --partition "hash:${toString partition}/4" --message-format json > "$out/shared-inventory.json"
                ${pkgs.python3}/bin/python3 ${./server/scripts/check_nextest_shards.py} --compare-partition \
                  ${archiveOutput}/shared-inventory.json.gz "$out/shared-inventory.json" \
                  > "$out/shared-coverage.json"
                ${pkgs.time}/bin/time -f 'WADDLE_CI_METRIC phase=tests shard=${toString partition} selection=whole elapsed_seconds=%e user_seconds=%U system_seconds=%S cpu=%P max_process_rss_kib=%M exit_code=%x' \
                  ${pkgs.cargo-nextest}/bin/cargo-nextest nextest run "''${reused_args[@]}" --test-threads 8 -E "$whole_filter"
                ${pkgs.time}/bin/time -f 'WADDLE_CI_METRIC phase=tests shard=${toString partition} selection=shared elapsed_seconds=%e user_seconds=%U system_seconds=%S cpu=%P max_process_rss_kib=%M exit_code=%x' \
                  ${pkgs.cargo-nextest}/bin/cargo-nextest nextest run "''${reused_args[@]}" --test-threads 8 -E "$shared_filter" --partition "hash:${toString partition}/4"
                runHook postCheck
              '';
            };
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
              ./server/README.md
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
          # Fail before compiling if a CI runner cannot create the namespaces
          # required by the four co-located test sandboxes.
          waddle-ci-sandbox-probe =
            pkgs.runCommand "waddle-ci-sandbox-probe"
              {
                preferLocalBuild = true;
                allowSubstitutes = false;
              }
              ''
                echo "WADDLE_CI_METRIC phase=sandbox_probe success=true"
                touch "$out"
              '';
          waddle-server-test-archive = serverTestArchive;
          waddle-server-test-shard-1 = mkServerTestShard 1;
          waddle-server-test-shard-2 = mkServerTestShard 2;
          waddle-server-test-shard-3 = mkServerTestShard 3;
          waddle-server-test-shard-4 = mkServerTestShard 4;

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
              ./server/README.md
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
            # reqwest's platform verifier needs explicit trust roots inside
            # the Nix sandbox, including tests that only construct a client.
            SSL_CERT_FILE = "${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt";
            WADDLE_CERTS_EPHEMERAL = "true";
            WADDLE_TEST_FIXED_ACCOUNT_ENABLED = "true";
            WADDLE_TEST_FIXED_ACCOUNT_PASSWORD = "cuenv-test-password";
            WADDLE_UPLOAD_DIR = "./uploads";
            RUST_BACKTRACE = "1";
          };
          testArgs =
            baseArgs
            // testRuntimeEnv
            // {
              # The Mimir-rules drift guard test (waddle-xmpp) reads the
              # rules file at <manifest>/../../../infrastructure/... — the
              # parent of the source root. Without this copy the guard
              # silently skips in every nix test lane, including the
              # dedicated xmpp unit/XEP lanes built from testArgs (#1436).
              postUnpack = ''
                mkdir -p "$sourceRoot/../infrastructure/waddle.cloud/rules/mimir"
                cp ${./infrastructure/waddle.cloud/rules/mimir/waddle-reliability.yaml} \
                  "$sourceRoot/../infrastructure/waddle.cloud/rules/mimir/waddle-reliability.yaml"
                # The CNPG monitoring-query drift test (waddle-server, Postgres
                # lane) reads the ConfigMap the same way (#1695).
                mkdir -p "$sourceRoot/../infrastructure/waddle.cloud/gitops/waddle-server"
                cp ${./infrastructure/waddle.cloud/gitops/waddle-server/postgresql-monitoring-ingress.yaml} \
                  "$sourceRoot/../infrastructure/waddle.cloud/gitops/waddle-server/postgresql-monitoring-ingress.yaml"
              '';
            };
          serverTestArgs = testArgs // {
            postUnpack = testArgs.postUnpack + ''
              cp -R ${./server/charts} "$sourceRoot/charts"
            '';
          };
          serverPostgresTestArgs = serverTestArgs // {
            nativeBuildInputs = serverTestArgs.nativeBuildInputs ++ [
              # Pin the major explicitly rather than tracking the nixpkgs
              # default (which moved 17 -> 18 in the 2026-09 snapshot), so a
              # nixpkgs bump cannot silently change the database the tests
              # run against. Prod runs the CloudNativePG operator default.
              pkgs.postgresql_17
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
              # The 200-round steal/veto stress test deliberately exercises
              # deadlocks. Detect cycles promptly without changing lock or
              # statement timeouts, transaction semantics, or test rounds.
              pg_ctl -D "$PGDATA" -o "-k $PGHOST -p $PGPORT -c listen_addresses= -c deadlock_timeout=50ms" -w start
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
          # Keep reusable dependency artifacts and vendored sources exposed
          # separately. Validation checks cache only their success output;
          # no downstream build consumes their full Cargo target directories.
          waddle-server-check-deps = workspaceAllFeaturesArtifacts;
          waddle-server-cargo-vendor = craneLib.vendorCargoDeps { src = serverCheckSrc; };
          waddle-server-fmt = craneLib.cargoFmt {
            pname = "waddle-server-fmt";
            version = "0.1.0";
            src = serverCheckSrc;
            cargoExtraArgs = "--all";
          };
          waddle-server-clippy = craneLib.cargoClippy (
            baseArgs
            // {
              doInstallCargoArtifacts = false;
              CARGO_PROFILE = "ci-test";
              cargoArtifacts = workspaceAllFeaturesArtifacts;
              cargoExtraArgs = "--locked --workspace --all-features";
              cargoClippyExtraArgs = "--all-targets -- -D warnings";
            }
          );
          waddle-server-test = craneLib.cargoNextest (
            serverPostgresTestArgs
            // {
              doInstallCargoArtifacts = false;
              CARGO_PROFILE = "ci-test";
              cargoArtifacts = workspaceAllFeaturesArtifacts;
              cargoExtraArgs = "--locked --workspace --all-features";
              cargoNextestExtraArgs = "--profile ci --lib --tests";
              # The waddle-server lib-test crate alone peaks at ~10 GB of
              # rustc RSS (measured 2026-09-13 in the ci-test profile;
              # codegen-units = 4 changes nothing, the peak is the frontend
              # and MIR of one very large test crate). Unbounded `-j nproc`
              # alongside the rest of the workspace (plus the derivation's
              # PostgreSQL instance) exceeds the 8x16 CI runner's memory on
              # cache-miss builds and gets rustc OOM-killed (SIGKILL, no
              # diagnostics). Four jobs was at the edge (#1774); two jobs
              # still let a second multi-GB test binary compile beside the
              # lib test and was killed on four of five runs (#1775). One
              # job serialises the workspace crates (deps arrive prebuilt
              # from the cached artifacts) so the single 10 GB peak is the
              # whole budget, and rustc still uses every core for codegen.
              CARGO_BUILD_JOBS = "1";
            }
          );
          waddle-server-doctest = craneLib.cargoTest (
            testArgs
            // {
              pname = "waddle-server-doctest";
              doInstallCargoArtifacts = false;
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
              doInstallCargoArtifacts = false;
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
              # Publish these exact immutable outputs instead of compiling
              # the same extensions again in the publication job.
              doInstallCargoArtifacts = false;
              installPhaseCommand = ''
                mkdir -p "$out/wasm"
                for module in ai_chatbot decision_polls github link_board stargate_quotes; do
                  wasm="''${CARGO_TARGET_DIR:-target}/wasm32-wasip2/release/$module.wasm"
                  test -s "$wasm"
                  cp "$wasm" "$out/wasm/$module.wasm"
                done
              '';
            }
          );
          waddle-server-xmpp-unit-tests = craneLib.cargoNextest (
            testArgs
            // {
              pname = "waddle-server-xmpp-unit-tests";
              doInstallCargoArtifacts = false;
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
              doInstallCargoArtifacts = false;
              CARGO_PROFILE = "ci-test";
              cargoArtifacts = serverTestArtifacts;
              cargoExtraArgs = "--locked --package waddle-server";
              cargoNextestExtraArgs = "--profile ci --lib --tests";
              checkPhase = ''
                runHook preCheck
                ${lib.optionalString pkgs.stdenv.hostPlatform.isLinux ''
                  export RUSTC_WORKSPACE_WRAPPER=${mkTestRustcWrapper pkgs}
                  echo "WADDLE_CI_METRIC lane=xmpp-server phase=compile linker=mold version=${pkgs.mold.version}"
                ''}
                # Match the archive's compiler improvements while retaining
                # this lane's default-feature coverage and dependency cache.
                nextest_args=(--cargo-profile "$CARGO_PROFILE" --locked --package waddle-server --profile ci --lib --tests
                  --config profile.ci-test.package.waddle-server.opt-level=0
                  --config profile.ci-test.package.waddle-xmpp.opt-level=0)
                echo "WADDLE_CI_METRIC lane=xmpp-server phase=compile waddle_server_opt_level=0 waddle_xmpp_opt_level=0 dependency_profile_unchanged=true"
                ${pkgs.time}/bin/time -f 'WADDLE_CI_METRIC lane=xmpp-server phase=compile elapsed_seconds=%e user_seconds=%U system_seconds=%S cpu=%P max_process_rss_kib=%M exit_code=%x' \
                  cargo nextest run "''${nextest_args[@]}" --no-run --timings
                ${pkgs.time}/bin/time -f 'WADDLE_CI_METRIC lane=xmpp-server phase=tests elapsed_seconds=%e user_seconds=%U system_seconds=%S cpu=%P max_process_rss_kib=%M exit_code=%x' \
                  cargo nextest run "''${nextest_args[@]}"
                runHook postCheck
              '';
            }
          );
          waddle-server-xmpp-cue-e2e = craneLib.cargoNextest (
            serverTestArgs
            // {
              pname = "waddle-server-xmpp-cue-e2e";
              doInstallCargoArtifacts = false;
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
              doInstallCargoArtifacts = false;
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
          # cue 0.17.1 does not terminate evaluating server/: `cue vet .`
          # takes ~4s on 0.16.1 and ran >11min/OOM on 0.17.1, which killed the
          # renderDeployment gate and would hang the main-only
          # publishContainerImage task. The two known v0.17 hang issues
          # (cue-lang/cue#4421, #4422) are fixed in 0.17.1, so this is a
          # distinct unreported regression: see #1763. Do not drop this hold
          # because an upstream issue looks closed -- measure `cue vet .`
          # in server/ on the candidate version first. Covers the CLI only;
          # the cuengine crate is pinned separately in server/Cargo.lock.
          cue = pkgs.cue.overrideAttrs (
            finalAttrs: _prev: {
              version = "0.16.1";
              src = pkgs.fetchFromGitHub {
                owner = "cue-lang";
                repo = "cue";
                tag = "v${finalAttrs.version}";
                hash = "sha256-mTj3XMWByNrKjm+/MOQGLyUKIv4JJ8i6Oaphbzls84U=";
              };
              vendorHash = "sha256-HXRrVPjPc10Q1MVr1d9vZBWgSVqNZ5J0UgvP/hTPfcg=";
            }
          );
        in
        {
          default = pkgs.mkShell {
            packages = [
              rustToolchain
              pkgs.bun
              pkgs.nodejs_22
              pkgs.python3
              pkgs.go
              cue
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
