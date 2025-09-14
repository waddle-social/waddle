{ pkgs, lib, config, inputs, ... }:
{
  packages = [ 
    pkgs.git
    pkgs.patchelf
  ];

  enterShell = ''
    ${pkgs.lib.optionalString pkgs.stdenv.isLinux ''
      # Patch wrangler workerd binary (Linux only)
      for dir in colony/website docs; do
        if [[ -d "$dir" ]]; then
          __patchTarget="$dir/node_modules/@cloudflare/workerd-linux-64/bin/workerd"
          if [[ -f "$__patchTarget" ]]; then
            ${pkgs.patchelf}/bin/patchelf --set-interpreter ${pkgs.glibc}/lib/ld-linux-x86-64.so.2 "$__patchTarget"
          fi
        fi
      done
    ''}
  '';
}
