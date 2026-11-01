#!/bin/sh
set -eu

# Fixed build artifacts only. readelf does not run either inspected executable.
for variant in probe probe-controlled; do
    LC_ALL=C readelf -hW "build/$variant" > "build/$variant-elf-header"
    LC_ALL=C readelf -lW "build/$variant" > "build/$variant-elf-program-headers"
    LC_ALL=C readelf -dW "build/$variant" > "build/$variant-elf-dynamic"

    rg -q 'Type: +DYN ' "build/$variant-elf-header"
    rg -q 'Class: +ELF64' "build/$variant-elf-header"
    rg -q 'GNU_STACK .* RW +0x' "build/$variant-elf-program-headers"
    rg -q 'GNU_RELRO ' "build/$variant-elf-program-headers"
    rg -q 'FLAGS.*BIND_NOW' "build/$variant-elf-dynamic"
    rg -q 'FLAGS_1.*PIE' "build/$variant-elf-dynamic"

    if rg -q ' INTERP |LOAD .* RWE |GNU_STACK .*RWE' "build/$variant-elf-program-headers"; then
        echo 'unexpected ELF interpreter or executable writable segment' >&2
        exit 1
    fi
    if rg -q '\((NEEDED|RPATH|RUNPATH|AUDIT|DEPAUDIT|FILTER|AUXILIARY)\)' "build/$variant-elf-dynamic"; then
        echo 'unexpected ELF dynamic dependency or loader search path' >&2
        exit 1
    fi
    echo "$variant static PIE verified: no interpreter/dependencies, RELRO, NOW, non-executable stack"
done
