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
  kernel = pkgs.linux_6_18;
  compiler = pkgs.pkgsStatic.stdenv.cc;
  compilerCommand = "${compiler}/bin/${compiler.targetPrefix}cc";
  closure = pkgs.closureInfo {
    rootPaths = [
      compiler
      pkgs.python3
    ];
  };
in
assert pkgs.stdenv.hostPlatform.system == "x86_64-linux";
pkgs.stdenvNoCC.mkDerivation {
  pname = "waddle-isolation-probe-artifact";
  version = "1";
  src = pkgs.lib.cleanSourceWith {
    src = ./.;
    filter =
      path: type:
      let
        name = builtins.baseNameOf path;
      in
      if type == "directory" then
        name != "__pycache__"
      else
        type == "regular"
        && builtins.any (suffix: pkgs.lib.hasSuffix suffix name) [
          ".c"
          ".h"
          ".py"
          ".nix"
        ];
  };
  nativeBuildInputs = [
    pkgs.python3
    compiler
  ];
  dontConfigure = true;
  dontBuild = true;
  # Inspection and manifest hashes describe final bytes; no later patch/strip.
  dontFixup = true;
  doCheck = true;
  checkPhase = ''
    runHook preCheck
    # Host tests link model.c only. The guest init is compiled but never run.
    ${compilerCommand} -std=c11 -O2 -Wall -Wextra -Wpedantic -Werror \
      model.c model_test.c -o model-test
    ./model-test
    PROBE_TEST_CC=${compilerCommand} ${pkgs.python3}/bin/python3 -m unittest -v test_artifact
    runHook postCheck
  '';
  installPhase = ''
    runHook preInstall
    ${pkgs.python3}/bin/python3 build.py \
      --cc ${compilerCommand} \
      --kernel ${kernel}/bzImage \
      --kernel-config ${kernel.configfile} \
      --kernel-source ${kernel.src} \
      --toolchain-registration ${closure}/registration \
      --repository-lock ${repositoryLock} \
      --out "$out"
    ${pkgs.python3}/bin/python3 inspect_artifact.py "$out/initramfs.cpio.gz"
    runHook postInstall
  '';
  meta = {
    description = "Build-only diskless isolation probe; no launcher or boot authorization";
    platforms = [ "x86_64-linux" ];
  };
}
