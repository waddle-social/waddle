# Test-only host shell for the federation interoperability harness. Every
# package comes from the repository's flake.lock; this adds no flake output
# and runs nothing in a build sandbox.
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
  # The Erlang runtime ejabberdctl itself starts, for a supervised epmd.
  ejabberdErlang = builtins.head (builtins.filter (input: (input.pname or "") == "erlang") pkgs.ejabberd.buildInputs);
in
# The pinned peers. ejabberd must be 25.08 or later (federation plan, W10);
# the flake's nixpkgs carries 26.07, so no override is needed.
assert pkgs.postgresql_17.version == "17.11";
assert pkgs.spicedb.version == "1.56.1";
assert pkgs.prosody.version == "13.0.6";
assert pkgs.ejabberd.version == "26.07";
assert pkgs.go.version == "1.26.7";
pkgs.mkShell {
  packages = [
    rust
    pkgs.bash
    pkgs.cacert
    pkgs.coreutils
    pkgs.ejabberd
    pkgs.git
    pkgs.go
    pkgs.openssl
    pkgs.pkg-config
    pkgs.postgresql_17
    pkgs.procps
    pkgs.prosody
    pkgs.protobuf
    pkgs.shellcheck
    pkgs.spicedb
  ];
  buildInputs = [ pkgs.openssl ];
  # waddle-server's cuengine dependency builds its cgo bridge on a cold Cargo
  # target; keep its compiler and download trust repository-pinned.
  GOTOOLCHAIN = "local";
  SSL_CERT_FILE = "${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt";
  NIX_SSL_CERT_FILE = "${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt";
  PROTOC = "${pkgs.protobuf}/bin/protoc";
  INTEROP_POSTGRES_BIN = "${pkgs.postgresql_17}/bin";
  INTEROP_SPICEDB_BIN = "${pkgs.spicedb}/bin/spicedb";
  INTEROP_PROSODY_BIN = "${pkgs.prosody}/bin";
  INTEROP_EJABBERD_BIN = "${pkgs.ejabberd}/bin";
  INTEROP_ERLANG_BIN = "${ejabberdErlang}/bin";
  INTEROP_OPENSSL_BIN = "${pkgs.openssl.bin}/bin/openssl";
  INTEROP_PROSODY_VERSION = pkgs.prosody.version;
  INTEROP_EJABBERD_VERSION = pkgs.ejabberd.version;
  INTEROP_POSTGRES_VERSION = pkgs.postgresql_17.version;
  INTEROP_SPICEDB_VERSION = pkgs.spicedb.version;
}
