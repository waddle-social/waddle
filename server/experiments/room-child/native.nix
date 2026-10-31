# Host-only native fixture, pinned by the repository flake.lock. No production output.
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
pkgs.mkShell {
  packages = [ rust pkgs.bash pkgs.coreutils pkgs.git pkgs.pkg-config pkgs.protobuf pkgs.postgresql_17 pkgs.go pkgs.python3 pkgs.cacert ];
  buildInputs = [ pkgs.openssl ];
  ROOM_CHILD_POSTGRES_BIN = "${pkgs.postgresql_17}/bin";
  SSL_CERT_FILE = "${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt";
  NIX_SSL_CERT_FILE = "${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt";
  PROTOC = "${pkgs.protobuf}/bin/protoc";
}
