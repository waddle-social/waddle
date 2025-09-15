{ pkgs, lib, config, inputs, ... }:
{
  languages.javascript.enable = true;
  languages.javascript.bun.enable = true;

  packages = [ 
    pkgs.git
    pkgs.patchelf
  ];

  enterShell = ''
    ${pkgs.lib.optionalString pkgs.stdenv.isLinux ''
      # Patch wrangler workerd binary (Linux only)
      for dir in colony/website docs; do
        if [[ -d "$dir" ]]; then
          # Check both possible workerd locations
          for workerd_path in \
            "$dir/node_modules/@cloudflare/workerd-linux-64/bin/workerd" \
            "$dir/node_modules/@astrojs/cloudflare/node_modules/wrangler/node_modules/workerd/node_modules/@cloudflare/workerd-linux-64/bin/workerd" \
            "node_modules/@astrojs/cloudflare/node_modules/wrangler/node_modules/workerd/node_modules/@cloudflare/workerd-linux-64/bin/workerd" \
            "node_modules/wrangler/node_modules/workerd/node_modules/@cloudflare/workerd-linux-64/bin/workerd"
          do
            if [[ -f "$workerd_path" ]]; then
              echo "Patching workerd at: $workerd_path"
              ${pkgs.patchelf}/bin/patchelf --set-interpreter ${pkgs.glibc}/lib/ld-linux-x86-64.so.2 "$workerd_path"
            fi
          done
        fi
      done
    ''}
  '';
}
