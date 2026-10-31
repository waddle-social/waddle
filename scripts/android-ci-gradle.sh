#!/usr/bin/env bash
# Bound the whole CI Gradle invocation, including Kotlin compilation and lint
# workers. Keep local developer defaults in gradle.properties unchanged.
set -euo pipefail

printf '%s\n' 'Android CI Gradle: heap=5 GiB, workers=2, project parallelism=off, Kotlin=in-process'
if command -v free >/dev/null 2>&1; then
  free -m
fi
for android_ci_memory_file in /sys/fs/cgroup/memory.max /sys/fs/cgroup/memory.current /sys/fs/cgroup/memory.peak /sys/fs/cgroup/memory.events; do
  if [[ -r "$android_ci_memory_file" ]]; then
    printf '%s\n' "$android_ci_memory_file"
    cat "$android_ci_memory_file"
  fi
done

exec ./gradlew \
  --no-daemon --no-parallel --max-workers=2 \
  -Pkotlin.compiler.execution.strategy=in-process \
  '-Dorg.gradle.jvmargs=-Xmx5g -XX:+UseParallelGC' \
  "$@"
