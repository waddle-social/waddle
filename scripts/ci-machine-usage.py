#!/usr/bin/env python3
"""Measure what a CI job uses of the machine it asked for.

  sample    append whole-machine CPU and memory samples until killed
  tee       copy stdin to stdout, and to a file with a timestamp per line
  report    reduce one job's samples and log to a JSON record on stdout
  summarize print a Markdown comparison of several records

The runner benchmark (ci/runner-benchmark) runs the Rust shard lane on several
Namespace machine shapes. `WADDLE_CI_METRIC` lines give each phase's duration;
the samples give the cores and memory the whole machine had busy meanwhile,
including PostgreSQL and the Nix daemon, which the lane's own per-process
figures leave out.
"""

import argparse
import json
import os
from pathlib import Path
import re
import sys
import time

# Namespace's Linux concurrency limit on the Team plan.
LIMIT_VCPU = 64
LIMIT_MEMORY_GIB = 128
METRIC = re.compile(r'WADDLE_CI_METRIC (.*)')


def confined(path):
    """Resolve a file argument, refusing anything outside the working
    directory and the runner's temporary directory."""
    resolved = Path(path).resolve()
    roots = [Path.cwd().resolve()]
    if os.environ.get('RUNNER_TEMP'):
        roots.append(Path(os.environ['RUNNER_TEMP']).resolve())
    if not any(resolved == root or root in resolved.parents for root in roots):
        raise SystemExit(f'{path}: outside the working directory and RUNNER_TEMP')
    return resolved


def read_sample(stat_text, meminfo_text, now):
    fields = stat_text.splitlines()[0].split()[1:]
    values = [int(value) for value in fields[:8]]
    idle = values[3] + values[4]  # idle + iowait
    memory = {key: value for key, _, value in (line.partition(':') for line in meminfo_text.splitlines()) if value}
    total = int(memory['MemTotal'].split()[0])
    available = int(memory['MemAvailable'].split()[0])
    cpus = sum(1 for line in stat_text.splitlines() if re.match(r'cpu\d+ ', line))
    return {'t': now, 'busy': sum(values) - idle, 'total': sum(values), 'cpus': cpus,
            'mem_used_kib': total - available, 'mem_total_kib': total}


def sample(output, interval):
    with confined(output).open('a', encoding='utf-8') as samples:
        while True:
            record = read_sample(Path('/proc/stat').read_text(), Path('/proc/meminfo').read_text(), time.time())
            samples.write(json.dumps(record) + '\n')
            samples.flush()
            time.sleep(interval)


def tee(log):
    with confined(log).open('a', encoding='utf-8') as copy:
        for line in sys.stdin:
            sys.stdout.write(line)
            sys.stdout.flush()
            copy.write(f'{time.time():.3f} {line}')
            copy.flush()


def load_samples(path):
    return [json.loads(line) for line in confined(path).read_text().splitlines() if line.strip()]


def usage(samples, start=None, end=None):
    """Busy cores and memory between two times; None when under two samples."""
    window = [item for item in samples
              if (start is None or item['t'] >= start) and (end is None or item['t'] <= end)]
    if len(window) < 2:
        return None
    cores = []
    for before, after in zip(window, window[1:]):
        total = after['total'] - before['total']
        if total > 0:
            cores.append((after['busy'] - before['busy']) / total * after['cpus'])
    if not cores:
        return None
    ordered = sorted(cores)
    return {
        'seconds': round(window[-1]['t'] - window[0]['t'], 1),
        'mean_busy_cores': round(sum(cores) / len(cores), 2),
        'p95_busy_cores': round(ordered[min(len(ordered) - 1, int(len(ordered) * 0.95))], 2),
        'max_busy_cores': round(ordered[-1], 2),
        'peak_memory_gib': round(max(item['mem_used_kib'] for item in window) / 1024 / 1024, 2),
    }


def parse_log(path):
    """Timestamped WADDLE_CI_METRIC lines as (time, {key: value}) pairs."""
    metrics = []
    for line in confined(path).read_text(errors='replace').splitlines():
        stamp, _, rest = line.partition(' ')
        match = METRIC.search(rest)
        if not match:
            continue
        try:
            when = float(stamp)
        except ValueError:
            continue
        metrics.append((when, {key: value for key, _, value in
                               (pair.partition('=') for pair in match.group(1).split()) if value}))
    return metrics


def report(args):
    samples = load_samples(args.samples)
    metrics = parse_log(args.log)
    compile_lines = [(when, values) for when, values in metrics if values.get('phase') == 'compile']
    compile_done = [(when, values) for when, values in compile_lines if 'elapsed_seconds' in values]
    affinity = [when for when, values in metrics if values.get('phase') == 'shard_affinity']
    tests = [(when, values) for when, values in metrics if values.get('phase') == 'tests']
    shard_seconds = {}
    for _, values in tests:
        shard = values.get('shard', '?')
        shard_seconds[shard] = round(shard_seconds.get(shard, 0) + float(values.get('elapsed_seconds', 0)), 1)
    record = {
        'variant': args.variant,
        'vcpu': args.vcpu,
        'memory_gib': args.memory_gib,
        'outcome': args.outcome,
        'lane_seconds': args.lane_seconds,
        'machine': usage(samples),
        'compile_seconds': float(compile_done[0][1]['elapsed_seconds']) if compile_done else None,
        'compile': usage(samples, compile_lines[0][0], compile_done[0][0]) if compile_done else None,
        'shard_seconds': shard_seconds,
        'tests': usage(samples, min(affinity), max(when for when, _ in tests)) if affinity and tests else None,
        'cpus_seen': samples[-1]['cpus'] if samples else None,
        'memory_total_gib': round(samples[-1]['mem_total_kib'] / 1024 / 1024, 1) if samples else None,
    }
    print(json.dumps(record, indent=2, sort_keys=True))


def cell(value, suffix=''):
    return '—' if value is None else f'{value}{suffix}'


def minutes(seconds):
    return None if seconds is None else round(seconds / 60, 1)


def summarize(directory):
    records = [json.loads(path.read_text()) for path in sorted(confined(directory).rglob('*.json'))]
    records = [record for record in records if isinstance(record, dict) and 'variant' in record]
    records.sort(key=lambda record: (-record['vcpu'], record['variant']))
    lines = [
        '## Runner shape benchmark: Rust shard lane',
        '',
        f'Concurrency limit assumed: {LIMIT_VCPU} vCPU / {LIMIT_MEMORY_GIB} GB (Namespace Team, Linux). '
        '"Fit" is how many of this job can run at once under it. '
        'vCPU-min is requested vCPUs x lane minutes: what the job holds of the limit.',
        '',
        '| Variant | Shape | Outcome | Lane min | Compile min | Slowest shard min | vCPU-min | Fit |'
        ' Busy cores compile (mean/max) | Busy cores tests (mean/max) | Peak memory GB |',
        '| --- | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |',
    ]
    for record in records:
        lane = minutes(record.get('lane_seconds'))
        slowest = max(record['shard_seconds'].values()) if record.get('shard_seconds') else None
        fit = min(LIMIT_VCPU // record['vcpu'], LIMIT_MEMORY_GIB // record['memory_gib'])

        def cores(phase):
            data = record.get(phase)
            return '—' if not data else f"{data['mean_busy_cores']} / {data['max_busy_cores']}"

        machine = record.get('machine') or {}
        lines.append(
            f"| {record['variant']} | {record['vcpu']}x{record['memory_gib']} | {record['outcome']} "
            f"| {cell(lane)} | {cell(minutes(record.get('compile_seconds')))} | {cell(minutes(slowest))} "
            f"| {cell(None if lane is None else round(lane * record['vcpu']))} | {fit} "
            f"| {cores('compile')} | {cores('tests')} | {cell(machine.get('peak_memory_gib'))} |")
    if not records:
        lines.append('| no records | | | | | | | | | | |')
    print('\n'.join(lines))


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    commands = parser.add_subparsers(dest='command', required=True)
    sampler = commands.add_parser('sample')
    sampler.add_argument('--output', required=True)
    sampler.add_argument('--interval', type=float, default=2.0)
    copier = commands.add_parser('tee')
    copier.add_argument('--log', required=True)
    reporter = commands.add_parser('report')
    for name in ('--samples', '--log', '--variant', '--outcome'):
        reporter.add_argument(name, required=True)
    reporter.add_argument('--vcpu', type=int, required=True)
    reporter.add_argument('--memory-gib', type=int, required=True)
    reporter.add_argument('--lane-seconds', type=float, required=True)
    summary = commands.add_parser('summarize')
    summary.add_argument('directory')
    args = parser.parse_args()
    if args.command == 'sample':
        sample(args.output, args.interval)
    elif args.command == 'tee':
        tee(args.log)
    elif args.command == 'report':
        report(args)
    else:
        summarize(args.directory)


if __name__ == '__main__':
    main()
