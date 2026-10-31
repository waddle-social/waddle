# Source/build recipe only. Its checks never execute the guard or manager tools.
let
  lock = builtins.fromJSON (builtins.readFile ../../../../flake.lock);
  pkgs = import (builtins.fetchTree lock.nodes.nixpkgs.locked) {
    system = "x86_64-linux";
  };
  compiler = pkgs.pkgsStatic.stdenv.cc;
in
pkgs.stdenvNoCC.mkDerivation {
  pname = "waddle-benign-resource-inspection";
  version = "1";
  src = pkgs.lib.cleanSourceWith {
    src = ./.;
    filter = path: type: (type == "directory" && builtins.baseNameOf path == "fixtures") || builtins.elem (builtins.baseNameOf path) [
      "resource_guard.c" "resource_guard.py" "test_resource_guard.py"
      "systemd-v262-failed-readback.txt"
    ];
  };
  nativeBuildInputs = [ compiler pkgs.python3 pkgs.binutils pkgs.ruff pkgs.ripgrep ];
  dontConfigure = true;
  dontFixup = true;
  buildPhase = ''
    mkdir build
    cat > build/resource_paths.h <<EOF
    #define RESOURCE_PYTHON "${pkgs.python3}/bin/python3"
    #define RESOURCE_SCRIPT "$out/libexec/host-controller/resource_guard.py"
    #define RESOURCE_GUARD "$out/libexec/host-controller/resource-guard"
    EOF
    flags="-std=c11 -O2 -Wall -Wextra -Werror -Wpedantic -Wconversion -Wshadow -Wformat=2 -Wstrict-prototypes -Wmissing-prototypes -fstack-protector-strong -fPIE -ffile-prefix-map=$PWD=."
    cc=${compiler}/bin/${compiler.targetPrefix}cc
    $cc $flags -Ibuild resource_guard.c -static-pie -Wl,-z,relro,-z,now,-z,noexecstack -o build/resource-guard
    $cc $flags -DRESOURCE_PARENT -Ibuild resource_guard.c -static-pie -Wl,-z,relro,-z,now,-z,noexecstack -o build/resource-inspection
    $cc $flags -Ibuild -fanalyzer -c resource_guard.c -o build/analyze-guard.o
    $cc $flags -DRESOURCE_PARENT -Ibuild -fanalyzer -c resource_guard.c -o build/analyze-parent.o
  '';
  doCheck = true;
  checkPhase = ''
    python3 -B -m unittest -v test_resource_guard.py
    ruff check resource_guard.py test_resource_guard.py
    ruff format --check resource_guard.py test_resource_guard.py
    for image in build/resource-guard build/resource-inspection; do
      readelf -h "$image" | rg 'Type:.*DYN'
      if readelf -l "$image" | rg 'INTERP'; then exit 1; fi
      if readelf -d "$image" | rg 'NEEDED'; then exit 1; fi
      readelf -lW "$image" | rg 'GNU_STACK.*RW '
      readelf -l "$image" | rg GNU_RELRO
      readelf -d "$image" | rg BIND_NOW
    done
  '';
  installPhase = ''
    mkdir -p "$out/bin" "$out/libexec/host-controller"
    install -m755 build/resource-inspection "$out/bin/benign-resource-inspection"
    install -m755 build/resource-guard "$out/libexec/host-controller/resource-guard"
    install -m644 resource_guard.py resource_guard.c "$out/libexec/host-controller/"
    cat > "$out/libexec/host-controller/resource-paths.json" <<EOF
    {"run":"${pkgs.systemd}/bin/systemd-run","ctl":"${pkgs.systemd}/bin/systemctl","guard":"$out/libexec/host-controller/resource-guard"}
    EOF
    (cd "$out"; sha256sum bin/benign-resource-inspection libexec/host-controller/* > source-and-artifact-sha256.txt)
  '';
  meta.platforms = [ "x86_64-linux" ];
}
