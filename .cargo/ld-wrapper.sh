#!/usr/bin/env bash
set -e

# Run the standard linker
/usr/bin/gcc "$@"

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

# If building a release binary, strip redundant unwinding tables
if [ -n "$output" ] && [ -f "$output" ] && [[ "$output" == *"release"* ]]; then
    /usr/bin/objcopy --remove-section .eh_frame --remove-section .eh_frame_hdr "$output" 2>/dev/null || true
fi
