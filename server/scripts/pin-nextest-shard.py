#!/usr/bin/env python3
"""Pin the calling build shell before PostgreSQL and nextest start.

Invoke directly from checkPhase, before preCheck, passing --parent-pid "$$",
without a pipeline or command substitution. The shell and every later child
inherit the selected CPUs. Each
co-located shard must start with the same allowed CPU set; Nix --cores only sets
a concurrency hint and does not provide this affinity isolation itself.
"""

import argparse
import os


def shard_cpus(allowed, partition, count, cores):
    if count < 1 or cores < 1 or not 1 <= partition <= count:
        raise ValueError("require a positive shard count and cores, with 1 <= partition <= count")
    allowed = sorted(set(allowed))
    required = count * cores
    if len(allowed) < required:
        raise ValueError(
            f"co-located nextest shards require at least {required} allowed CPUs "
            f"({count} shards x {cores} CPUs), but this build sees {len(allowed)}; "
            "use the configured larger runner rather than sharing CPUs between shards"
        )
    offset = (partition - 1) * cores
    return set(allowed[offset:offset + cores])


def pin_parent(partition, count, cores, expected_parent):
    parent = os.getppid()
    if expected_parent < 1 or parent != expected_parent:
        raise ValueError("invoke the affinity helper directly from the build shell with --parent-pid \"$$\"")
    allowed = os.sched_getaffinity(0)
    selected = shard_cpus(allowed, partition, count, cores)
    if os.sched_getaffinity(parent) != allowed:
        raise ValueError("the calling build shell and affinity helper have different allowed CPUs")
    os.sched_setaffinity(parent, selected)
    if os.sched_getaffinity(parent) != selected:
        raise ValueError("the calling build shell did not retain the requested shard CPU affinity")
    print(
        f"WADDLE_CI_METRIC phase=shard_affinity shard={partition} "
        f"allowed_cpu_count={len(allowed)} assigned_cpu_count={len(selected)} "
        f"cpu_ids={','.join(map(str, sorted(selected)))}",
        flush=True,
    )


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--parent-pid", type=int, required=True)
    parser.add_argument("--partition", type=int, required=True)
    parser.add_argument("--count", type=int, default=4)
    parser.add_argument("--cores", type=int, default=8)
    args = parser.parse_args()
    try:
        pin_parent(args.partition, args.count, args.cores, args.parent_pid)
    except (AttributeError, OSError, ValueError) as error:
        parser.exit(1, f"cannot pin nextest shard: {error}\n")


if __name__ == "__main__":
    main()
