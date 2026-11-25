# cuenv 0.77 keys tasks on changes made by nix print-dev-env. Nix appends
# the caller's PATH, including the Actions bootstrap's random directory.
if [[ ${GITHUB_ACTIONS:-} == true && -n ${CUENV_REMOTE_CACHE:-} ]]; then
  # Keep the pinned CLI available for checks that run on the host. Its
  # download directory is random, while RUNNER_TEMP is fixed on this profile.
  _waddle_host_bin="${RUNNER_TEMP:?}/waddle-cuenv-bin"
  _waddle_cuenv="$(type -P cuenv)"
  mkdir -p -m 700 "$_waddle_host_bin"
  if [[ ! "$_waddle_host_bin/cuenv" -ef "$_waddle_cuenv" ]]; then
    ln -sfnT "$_waddle_cuenv" "$_waddle_host_bin/cuenv"
  fi
  export PATH="${PATH%:"$nix_saved_PATH"}:$_waddle_host_bin:/nix/var/nix/profiles/default/bin:/usr/local/bin:/usr/bin:/bin"
  unset _waddle_host_bin _waddle_cuenv
fi
