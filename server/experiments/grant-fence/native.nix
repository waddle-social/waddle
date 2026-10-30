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
pkgs.mkShell {
  packages = [
    rust
    pkgs.bash
    pkgs.git
    pkgs.pkg-config
    pkgs.protobuf
    pkgs.postgresql_17
    pkgs.spicedb
  ];
  buildInputs = [ pkgs.openssl ];
  FENCE_POSTGRES_BIN = "${pkgs.postgresql_17}/bin";
  FENCE_SPICEDB_BIN = "${pkgs.spicedb}/bin/spicedb";
  FENCE_TEST_SHELL = "${pkgs.bash}/bin/bash";
  FENCE_TEST_SLEEP = "${pkgs.coreutils}/bin/sleep";
  PROTOC = "${pkgs.protobuf}/bin/protoc";
}
