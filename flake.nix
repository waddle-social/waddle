{
  description = "Waddle Social monorepo";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    rust-overlay.url = "github:oxalica/rust-overlay";
    rust-overlay.inputs.nixpkgs.follows = "nixpkgs";
  };

  outputs = { self, nixpkgs, rust-overlay }:
    let
      systems = [
        "aarch64-darwin"
        "x86_64-darwin"
        "aarch64-linux"
        "x86_64-linux"
      ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f system);
    in
    {
      devShells = forAllSystems (system:
        let
          overlays = [ rust-overlay.overlays.default ];
          pkgs = import nixpkgs { inherit system overlays; };
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
              pkgs.protobuf
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
