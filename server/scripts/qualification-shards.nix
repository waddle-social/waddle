{ sourcePath, candidateSha, runId, system ? builtins.currentSystem }:
assert builtins.match "[0-9a-f]{40}" candidateSha != null;
assert builtins.match "[0-9a-f]{64}" runId != null;
let
  flake = builtins.getFlake "path:${sourcePath}";
  mkShard = partition:
    let
      shard = flake.packages.${system}."waddle-server-test-shard-${toString partition}";
      qualified = shard.overrideAttrs (_: {
        waddleQualificationCandidateSha = candidateSha;
        waddleQualificationRunId = runId;
        waddleQualificationSource = builtins.storePath sourcePath;
        preferLocalBuild = true;
        allowSubstitutes = false;
      });
    in qualified;
in map mkShard [ 1 2 3 4 ]
