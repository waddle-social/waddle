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
      waddleGitSha = self.rev or (self.dirtyRev or "unknown");
    in
    {
      packages = forAllSystems (
        system:
        let
          pkgs = mkPkgs system;
          lib = pkgs.lib;
          rustToolchain = pkgs.rust-bin.fromRustupToolchainFile ./server/rust-toolchain.toml;
          craneLib = (crane.mkLib pkgs).overrideToolchain rustToolchain;
          serverSrc = lib.fileset.toSource {
            root = ./server;
            fileset = lib.fileset.unions [
              ./server/Cargo.toml
              ./server/Cargo.lock
              ./server/crates
              ./server/wit
            ];
          };
          commonArgs = {
            pname = "waddle-server";
            version = "0.1.0";
            src = serverSrc;
            strictDeps = true;
            cargoExtraArgs = "--locked --package waddle-server --bin waddle-server";
            WADDLE_GIT_SHA = waddleGitSha;
            WADDLE_CERTS_EPHEMERAL = "true";
            WADDLE_TEST_FIXED_ACCOUNT_ENABLED = "true";
            WADDLE_TEST_FIXED_ACCOUNT_PASSWORD = "cuenv-test-password";
            WADDLE_UPLOAD_DIR = "./uploads";
            RUST_BACKTRACE = "1";
            nativeBuildInputs = [
              pkgs.pkg-config
              pkgs.protobuf
            ];
            buildInputs = [
              pkgs.openssl
              pkgs.sqlite
            ];
          };
          cargoArtifacts = craneLib.buildDepsOnly (
            commonArgs
            // {
              doCheck = false;
              cargoCheckExtraArgs = "";
            }
          );
          waddle-server = craneLib.buildPackage (
            commonArgs
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
          serverSrc = lib.fileset.toSource {
            root = ./server;
            fileset = lib.fileset.unions [
              ./server/Cargo.toml
              ./server/Cargo.lock
              ./server/crates
              ./server/wit
            ];
          };
          commonArgs = {
            pname = "waddle-server";
            version = "0.1.0";
            src = serverSrc;
            strictDeps = true;
            WADDLE_GIT_SHA = waddleGitSha;
            WADDLE_CERTS_EPHEMERAL = "true";
            WADDLE_TEST_FIXED_ACCOUNT_ENABLED = "true";
            WADDLE_TEST_FIXED_ACCOUNT_PASSWORD = "cuenv-test-password";
            WADDLE_UPLOAD_DIR = "./uploads";
            RUST_BACKTRACE = "1";
            nativeBuildInputs = [
              pkgs.pkg-config
              pkgs.protobuf
            ];
            buildInputs = [
              pkgs.openssl
              pkgs.sqlite
            ];
          };
          workspaceArtifacts = craneLib.buildDepsOnly (
            commonArgs
            // {
              cargoExtraArgs = "--locked --workspace";
              cargoCheckExtraArgs = "--all-targets";
              cargoBuildExtraArgs = "--all-targets";
              cargoTestExtraArgs = "--all-targets --no-run";
            }
          );
          workspaceAllFeaturesArtifacts = craneLib.buildDepsOnly (
            commonArgs
            // {
              cargoExtraArgs = "--locked --workspace --all-features";
              cargoCheckExtraArgs = "--all-targets";
              cargoBuildExtraArgs = "--all-targets";
              cargoTestExtraArgs = "--all-targets --no-run";
            }
          );
          xmppArtifacts = craneLib.buildDepsOnly (
            commonArgs
            // {
              cargoExtraArgs = "--locked --package waddle-xmpp";
              cargoCheckExtraArgs = "--all-targets";
              cargoBuildExtraArgs = "--all-targets";
              cargoTestExtraArgs = "--all-targets --no-run";
            }
          );
          serverTestArtifacts = craneLib.buildDepsOnly (
            commonArgs
            // {
              cargoExtraArgs = "--locked --package waddle-server";
              cargoCheckExtraArgs = "--all-targets";
              cargoBuildExtraArgs = "--all-targets";
              cargoTestExtraArgs = "--all-targets --no-run";
            }
          );
        in
        {
          waddle-server-build = self.packages.${system}.waddle-server;
          waddle-server-fmt = craneLib.cargoFmt {
            pname = "waddle-server-fmt";
            version = "0.1.0";
            src = serverSrc;
            cargoExtraArgs = "--all";
          };
          waddle-server-clippy = craneLib.cargoClippy (
            commonArgs
            // {
              cargoArtifacts = workspaceAllFeaturesArtifacts;
              cargoExtraArgs = "--locked --workspace --all-features";
              cargoClippyExtraArgs = "--all-targets -- -D warnings";
            }
          );
          waddle-server-test = craneLib.cargoTest (
            commonArgs
            // {
              cargoArtifacts = workspaceArtifacts;
              cargoExtraArgs = "--locked --workspace";
              cargoTestExtraArgs = "--all-targets";
            }
          );
          waddle-server-xmpp-unit-tests = craneLib.cargoTest (
            commonArgs
            // {
              pname = "waddle-server-xmpp-unit-tests";
              cargoArtifacts = xmppArtifacts;
              cargoExtraArgs = "--locked --package waddle-xmpp";
              cargoTestExtraArgs = "--lib --verbose";
            }
          );
          waddle-server-xmpp-server-tests = craneLib.cargoTest (
            commonArgs
            // {
              pname = "waddle-server-xmpp-server-tests";
              cargoArtifacts = serverTestArtifacts;
              cargoExtraArgs = "--locked --package waddle-server";
              cargoTestExtraArgs = "--verbose";
            }
          );
          waddle-server-xmpp-cue-e2e = craneLib.cargoTest (
            commonArgs
            // {
              pname = "waddle-server-xmpp-cue-e2e";
              cargoArtifacts = serverTestArtifacts;
              cargoExtraArgs = "--locked --package waddle-server";
              cargoTestExtraArgs = "--test xmpp_e2e_cue --verbose";
            }
          );
          waddle-server-xmpp-xep-integration = craneLib.cargoTest (
            commonArgs
            // {
              pname = "waddle-server-xmpp-xep-integration";
              cargoArtifacts = xmppArtifacts;
              cargoExtraArgs = "--locked --package waddle-xmpp";
              cargoTestExtraArgs = "--tests --verbose";
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
              pkgs.just
              pkgs.jujutsu
              pkgs.cargo-chef
              pkgs.teleport
              pkgs.openssl
              pkgs.pkg-config
              pkgs.protobuf
              pkgs.oras
              pkgs.fluxcd
              pkgs.yq-go
            ];

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
