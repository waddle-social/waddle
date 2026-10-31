# Separate gated reader package. Tests execute fake forked Python children only.
let
  lock = builtins.fromJSON (builtins.readFile ../../../../flake.lock);
  pkgs = import (builtins.fetchTree lock.nodes.nixpkgs.locked) {
    system = "x86_64-linux";
  };
in
pkgs.stdenvNoCC.mkDerivation {
  pname = "waddle-benign-resource-reader";
  version = "1";
  src = pkgs.lib.cleanSourceWith {
    src = ./.;
    filter = path: type: builtins.elem (builtins.baseNameOf path) [
      "resource_reader.py" "test_resource_reader.py" "resource_guard.py"
    ];
  };
  nativeBuildInputs = [ pkgs.python3 pkgs.ruff ];
  dontConfigure = true;
  dontBuild = true;
  dontFixup = true;
  doCheck = true;
  checkPhase = ''
    python3 -B -m unittest -v test_resource_reader.py
    ruff check resource_reader.py test_resource_reader.py
    ruff format --check resource_reader.py test_resource_reader.py
  '';
  installPhase = ''
    mkdir -p "$out/bin" "$out/libexec/host-controller"
    install -m644 resource_reader.py "$out/libexec/host-controller/"
    cat > "$out/bin/capture-benign-resource-inspection" <<EOF
    #!${pkgs.runtimeShell}
    exec ${pkgs.python3}/bin/python3 -I -S "$out/libexec/host-controller/resource_reader.py" "\$@"
    EOF
    chmod 755 "$out/bin/capture-benign-resource-inspection"
    (cd "$out"; sha256sum bin/capture-benign-resource-inspection libexec/host-controller/resource_reader.py > source-and-artifact-sha256.txt)
  '';
  meta.platforms = [ "x86_64-linux" ];
}
