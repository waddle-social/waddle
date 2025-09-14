{ pkgs, lib, config, inputs, ... }:
{
  packages = with pkgs; [
    biome
  ];

  languages.javascript = {
    enable = true;
    bun.enable = true;
  };

  languages.typescript.enable = true;

   enterShell = ''
    # Run bun install if node_modules doesn't exist or package.json is newer
    if [ ! -d "node_modules" ] || [ "package.json" -nt "node_modules" ]; then
      echo "Running bun install..."
      bun install
    fi

    # Patch workerd binary after install
    WORKERD_BIN="node_modules/@cloudflare/workerd-linux-64/bin/workerd"
    if [ -f "$WORKERD_BIN" ]; then
      # Check if already patched
      CURRENT_INTERPRETER=$(patchelf --print-interpreter "$WORKERD_BIN" 2>/dev/null || echo "")
      NIX_INTERPRETER="${pkgs.stdenv.cc.libc}/lib/ld-linux-x86-64.so.2"

      if [ "$CURRENT_INTERPRETER" != "$NIX_INTERPRETER" ]; then
        echo "Patching workerd binary for NixOS..."
        patchelf --set-interpreter "$NIX_INTERPRETER" "$WORKERD_BIN"
        patchelf --set-rpath "${pkgs.lib.makeLibraryPath [ pkgs.stdenv.cc.cc.lib pkgs.stdenv.cc.libc ]}" "$WORKERD_BIN"
        echo "Workerd binary patched successfully"
      fi
    fi
  '';
}
