{
  repositoryLock ? ../../../../flake.lock,
  pkgs ?
    import
      (builtins.fetchTree (builtins.fromJSON (builtins.readFile repositoryLock)).nodes.nixpkgs.locked)
      {
        system = "x86_64-linux";
      },
}:
let
  compiler = pkgs.pkgsStatic.stdenv.cc;
  compilerCommand = "${compiler}/bin/${compiler.targetPrefix}cc";
in
assert pkgs.stdenv.hostPlatform.system == "x86_64-linux";
pkgs.stdenvNoCC.mkDerivation {
  pname = "waddle-host-inspection-probe";
  version = "1";
  src = pkgs.lib.cleanSourceWith {
    src = ./.;
    filter =
      path: type:
      let
        name = builtins.baseNameOf path;
      in
      if type == "directory" then
        name != "build" && name != "__pycache__"
      else
        type == "regular"
        && (
          name == "Makefile"
          || builtins.any (suffix: pkgs.lib.hasSuffix suffix name) [
            ".c"
            ".h"
            ".py"
            ".sh"
          ]
        );
  };
  nativeBuildInputs = [
    compiler
    pkgs.binutils
    pkgs.gnumake
    pkgs.python3
    pkgs.ripgrep
  ];
  dontConfigure = true;
  dontFixup = true;
  buildPhase = ''
    runHook preBuild
    make STATIC_CC=${compilerCommand} all
    runHook postBuild
  '';
  doCheck = true;
  checkPhase = ''
    runHook preCheck
    # Only synthetic fixtures run; both inspection probes are inspected as ELF data.
    make STATIC_CC=${compilerCommand} check check-static check-analysis
    runHook postCheck
  '';
  installPhase = ''
    runHook preInstall
    install -Dm755 build/probe "$out/bin/host-inspection-probe"
    install -Dm755 build/probe-controlled "$out/bin/host-inspection-probe-controlled"
    runHook postInstall
  '';
  meta = {
    description = "Build-only host confinement inspection probe";
    platforms = [ "x86_64-linux" ];
  };
}
