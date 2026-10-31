let
  lock = builtins.fromJSON (builtins.readFile ../../../../flake.lock);
  pkgs = import (builtins.fetchTree lock.nodes.nixpkgs.locked) {
    system = "x86_64-linux";
  };
  bubblewrap = pkgs.bubblewrap.overrideAttrs (_old: {
    version = "0.12.0";
    src = pkgs.fetchFromGitHub {
      owner = "containers";
      repo = "bubblewrap";
      rev = "v0.12.0";
      hash = "sha256-VnhJ5bej3/GTHcU8+AkyR7f3J0KKDuoc94SFxo4grhk=";
    };
  });
  compiler = pkgs.pkgsStatic.stdenv.cc;
  compilerCommand = "${compiler}/bin/${compiler.targetPrefix}cc";
  probe = import ../host-canary/default.nix { inherit pkgs; };
  runtimePaths = pkgs.writeText "host-canary-runtime-paths.json" (
    builtins.toJSON {
      python = "${pkgs.python3}/bin/python3";
      systemd-run = "${pkgs.systemd}/bin/systemd-run";
      systemctl = "${pkgs.systemd}/bin/systemctl";
      bwrap = "${bubblewrap}/bin/bwrap";
      probe = "${probe}/bin/host-inspection-probe-controlled";
      decoder = "${../host-canary/frame.py}";
    }
  );
in
pkgs.stdenvNoCC.mkDerivation {
  pname = "waddle-host-confinement-canary";
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
    pkgs.ruff
  ];
  dontConfigure = true;
  dontFixup = true;
  buildPhase = ''
    runHook preBuild
    mkdir build
    cp ${runtimePaths} build/runtime-paths.json
    python3 -B build_manifest.py
    make STATIC_CC=${compilerCommand} all
    runHook postBuild
  '';
  doCheck = true;
  checkPhase = ''
    runHook preCheck
    # Only pure filter fixtures and integration fakes run, never the canary.
    make STATIC_CC=${compilerCommand} check check-analysis check-static
    ruff check *.py
    ruff format --check *.py
    runHook postCheck
  '';
  installPhase = ''
    runHook preInstall
    mkdir -p "$out/bin" "$out/libexec/host-controller"
    install -m755 build/bootstrap "$out/bin/host-confinement-canary"
    install -m755 build/witness "$out/libexec/host-controller/witness"
    install -m644 controller.py runtime.py policy.py observe.py \
      bootstrap.c witness.c filter.c filter.h sentinel_paths.h "$out/libexec/host-controller/"
    python3 -B build_manifest.py
    runHook postInstall
  '';
  meta = {
    description = "Fixed bounded host confinement canary; separate review required before execution";
    platforms = [ "x86_64-linux" ];
  };
}
