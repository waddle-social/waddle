# Test-only host shell. Package inputs come from the repository's flake.lock;
# this does not add a production flake output or execute tests in a build sandbox.
let
  repository = builtins.getFlake (toString ../../..);
  pkgs = import repository.inputs.nixpkgs {
    system = "x86_64-linux";
    overlays = [ repository.inputs.rust-overlay.overlays.default ];
  };
  toolchain = builtins.fromTOML (builtins.readFile ../../rust-toolchain.toml);
  rust = pkgs.rust-bin.stable.${toolchain.toolchain.channel}.default.override {
    extensions = toolchain.toolchain.components;
    targets = [ ];
  };
in
assert pkgs.postgresql_17.version == "17.11";
assert pkgs.spicedb.version == "1.56.1";
assert pkgs.go.version == "1.26.7";
pkgs.mkShell {
  packages = [
    rust
    pkgs.bash
    pkgs.cacert
    pkgs.git
    pkgs.go
    pkgs.pkg-config
    pkgs.protobuf
    pkgs.postgresql_17
    pkgs.spicedb
  ];
  buildInputs = [ pkgs.openssl ];
  # waddle-server's cuengine dev dependency builds its cgo bridge on a cold
  # Cargo target. Keep both its compiler and download trust repository-pinned.
  GOTOOLCHAIN = "local";
  SSL_CERT_FILE = "${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt";
  NIX_SSL_CERT_FILE = "${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt";
  FENCE_RUST_BIN = "${rust}/bin";
  FENCE_POSTGRES_BIN = "${pkgs.postgresql_17}/bin";
  FENCE_SPICEDB_BIN = "${pkgs.spicedb}/bin/spicedb";
  FENCE_TEST_SHELL = "${pkgs.bash}/bin/bash";
  FENCE_TEST_SLEEP = "${pkgs.coreutils}/bin/sleep";
  PROTOC = "${pkgs.protobuf}/bin/protoc";
}
