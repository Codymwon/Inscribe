#!/usr/bin/env bash
set -e

# Run the standard linker with fallback to CC or gcc
CC="${CC:-gcc}"
"$CC" "$@"

# Find the output binary from the -o argument
output=""
prev=""
for arg in "$@"; do
    if [ "$prev" = "-o" ]; then
        output="$arg"
        break
    fi
    prev="$arg"
done

# If building a release standalone binary, strip redundant unwinding tables if objcopy is available.
# Exclude test runners (which link libtest) to preserve test backtraces and panic unwinding.
is_test=0
for arg in "$@"; do
    if [[ "$arg" == *"libtest"* ]] || [[ "$arg" == *"test"* ]] || [[ "$arg" == *"task"* ]]; then
        is_test=1
        break
    fi
done

if [ -n "$output" ] && [ -f "$output" ] && [[ "$output" == *"release"* ]] && [ "$is_test" -eq 0 ]; then
    if command -v objcopy >/dev/null 2>&1; then
        objcopy --remove-section .eh_frame --remove-section .eh_frame_hdr "$output" 2>/dev/null || true
    fi
fi
