#!/bin/sh
# Inspect ELF files as data. Never execute the bootstrap or witness.
set -eu
for artifact in bootstrap witness; do
    readelf --wide --file-header "build/$artifact" > "build/$artifact.header"
    readelf --wide --program-headers "build/$artifact" > "build/$artifact.program"
    readelf --wide --dynamic "build/$artifact" > "build/$artifact.dynamic"
    rg -q 'Type:.*DYN' "build/$artifact.header"
    rg -q 'Machine:.*X86-64' "build/$artifact.header"
    if rg -q 'INTERP' "build/$artifact.program"; then exit 1; fi
    if rg -q 'NEEDED|RPATH|RUNPATH' "build/$artifact.dynamic"; then exit 1; fi
    rg -q 'GNU_RELRO' "build/$artifact.program"
    rg -q 'GNU_STACK.* RW ' "build/$artifact.program"
    rg -q 'BIND_NOW|FLAGS_1.*NOW' "build/$artifact.dynamic"
done
