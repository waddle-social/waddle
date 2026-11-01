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
  compilerFlags = builtins.fromJSON (builtins.readFile ./compiler-flags.json);
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
          ".json"
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
    source_a="$NIX_BUILD_TOP/independent-a"
    source_b="$NIX_BUILD_TOP/independent-b"
    (
      umask 022
      mkdir "$source_a"
      cp init.c model.c model.h "$source_a/"
      touch -d @1 "$source_a/init.c" "$source_a/model.c" "$source_a/model.h"
      ${pkgs.coreutils}/bin/timeout --kill-after=5s 120s \
        ${pkgs.coreutils}/bin/env -i PATH="$PATH" LC_ALL=C TZ=UTC SOURCE_DATE_EPOCH=0 NIX_HARDENING_ENABLE="" \
        ${compilerCommand} ${pkgs.lib.escapeShellArgs compilerFlags} \
        -ffile-prefix-map="$source_a"=. -fdebug-prefix-map="$source_a"=. \
        "$source_a/init.c" "$source_a/model.c" -o "$source_a/init-a"
    )
    (
      umask 077
      mkdir "$source_b"
      cp model.h model.c init.c "$source_b/"
      touch -d @900000000 "$source_b/init.c" "$source_b/model.c" "$source_b/model.h"
      ${pkgs.coreutils}/bin/timeout --kill-after=5s 120s \
        ${pkgs.coreutils}/bin/env -i PATH="$PATH" LC_ALL=C TZ=UTC SOURCE_DATE_EPOCH=0 NIX_HARDENING_ENABLE="" \
        ${compilerCommand} ${pkgs.lib.escapeShellArgs compilerFlags} \
        -ffile-prefix-map="$source_b"=. -fdebug-prefix-map="$source_b"=. \
        "$source_b/init.c" "$source_b/model.c" -o "$source_b/init-b"
    )
    PROBE_TEST_BINARY_A="$source_a/init-a" PROBE_TEST_BINARY_B="$source_b/init-b" \
      ${pkgs.python3}/bin/python3 -m unittest -v test_artifact
    runHook postCheck
  '';
  installPhase = ''
    runHook preInstall
    # Compiler inputs are fixed by this derivation; no user CLI selects a program.
    ${pkgs.coreutils}/bin/timeout --kill-after=5s 120s \
      ${pkgs.coreutils}/bin/env -i PATH="$PATH" LC_ALL=C TZ=UTC SOURCE_DATE_EPOCH=0 NIX_HARDENING_ENABLE="" \
      ${compilerCommand} ${pkgs.lib.escapeShellArgs compilerFlags} \
      -ffile-prefix-map="$PWD"=. -fdebug-prefix-map="$PWD"=. \
      "$PWD/init.c" "$PWD/model.c" -o "$NIX_BUILD_TOP/probe-init"
    ${pkgs.python3}/bin/python3 - <<'PY'
    import os
    from pathlib import Path
    from build import build_artifact

    build_artifact(
        init=Path(os.environ["NIX_BUILD_TOP"]) / "probe-init",
        compiler=Path(${builtins.toJSON compilerCommand}),
        compiler_flags=${builtins.toJSON compilerFlags},
        kernel=Path(${builtins.toJSON "${kernel}/bzImage"}),
        kernel_config=Path(${builtins.toJSON (toString kernel.configfile)}),
        kernel_source=Path(${builtins.toJSON (toString kernel.src)}),
        toolchain_registration=Path(${builtins.toJSON "${closure}/registration"}),
        repository_lock=Path("${repositoryLock}"),
        output=Path(os.environ["out"]),
    )
    PY
    ${pkgs.python3}/bin/python3 inspect_artifact.py "$out/initramfs.cpio.gz"
    runHook postInstall
  '';
  meta = {
    description = "Build-only diskless isolation probe; no launcher or boot authorization";
    platforms = [ "x86_64-linux" ];
  };
}
