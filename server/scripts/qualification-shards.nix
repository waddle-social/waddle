# `lane` selects a ci/runner-benchmark shard lane (flake.nix
# benchmarkShardLanes); the empty default is the production lane.
{ sourcePath, candidateSha, runId, lane ? "", system ? builtins.currentSystem }:
assert builtins.match "([0-9a-z]+(-[0-9a-z]+)*)?" lane != null;
assert builtins.match "[0-9a-f]{40}" candidateSha != null;
assert builtins.match "[0-9a-f]{64}" runId != null;
let
  flake = builtins.getFlake "path:${sourcePath}";
  mkShard = partition:
    let
      shard = flake.packages.${system}."waddle-server-test-shard-${toString partition}${if lane == "" then "" else "-bench-${lane}"}";
      qualified = shard.overrideAttrs (_: {
        waddleQualificationCandidateSha = candidateSha;
        waddleQualificationRunId = runId;
        waddleQualificationSource = builtins.storePath sourcePath;
        preferLocalBuild = true;
        allowSubstitutes = false;
      });
    in qualified;
in map mkShard [ 1 2 3 4 ]
