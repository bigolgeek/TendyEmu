#!/usr/bin/env bash
# Fetches the public test ROMs listed in scripts/test-roms.txt into roms/test/ (or into
# $TENDY_TEST_ROMS when it is set) and unpacks the SNES suite into snes-test-roms/.
#
# Nothing downloaded here is ever committed: ROM images are gitignored and this script only
# writes inside the destination folder. Every file is checked against the sha256 and byte count
# in the list; a file already present with a different checksum is refused, never overwritten.
#
# Usage: scripts/fetch-test-roms.sh
#        TENDY_TEST_ROMS=/path/to/roms scripts/fetch-test-roms.sh
set -uo pipefail

# `curl` and the other native tools need a Windows-style path: `pwd -W` under git-bash, plain
# `pwd` everywhere else.
here=$(cd "$(dirname "$0")/.." && pwd -W 2>/dev/null) || here=$(cd "$(dirname "$0")/.." && pwd)
list="$here/scripts/test-roms.txt"
dest="${TENDY_TEST_ROMS:-$here/roms/test}"

if [ ! -f "$list" ]; then
    echo "fetch-test-roms: missing $list" >&2
    exit 2
fi

mkdir -p "$dest" || exit 2

downloaded=0
present=0
local_only=0
failed=0

sha_of() { sha256sum "$1" | cut -d' ' -f1; }
size_of() { wc -c < "$1" | tr -d '[:space:]'; }
trim() { printf '%s' "$1" | tr -d '[:space:]'; }

while IFS='|' read -r name url sha bytes licence; do
    case "$name" in
        '' | '#'*) continue ;;
    esac
    name=$(trim "$name")
    url=$(trim "$url")
    sha=$(trim "$sha")
    bytes=$(trim "$bytes")
    target="$dest/$name"
    part="$target.part"

    if [ "$sha" = "-" ]; then
        printf 'LOCAL   %s: owner-supplied, not fetched (%s)\n' "$name" "${licence%%,*}"
        local_only=$((local_only + 1))
        continue
    fi

    mkdir -p "$(dirname "$target")" || exit 2

    if [ -f "$target" ]; then
        have=$(sha_of "$target")
        if [ "$have" = "$sha" ]; then
            printf 'present %s\n' "$name"
            present=$((present + 1))
            continue
        fi
        printf 'REFUSED %s: already present with sha256 %s, list wants %s\n' \
            "$name" "$have" "$sha" >&2
        failed=$((failed + 1))
        continue
    fi

    if ! curl -fsSL --retry 2 --max-time 180 -o "$part" "$url"; then
        printf 'FAILED  %s: download failed (%s)\n' "$name" "$url" >&2
        rm -f "$part"
        failed=$((failed + 1))
        continue
    fi

    have=$(sha_of "$part")
    size=$(size_of "$part")
    if [ "$have" != "$sha" ] || [ "$size" != "$bytes" ]; then
        printf 'FAILED  %s: got sha256 %s / %s bytes, list wants %s / %s\n' \
            "$name" "$have" "$size" "$sha" "$bytes" >&2
        rm -f "$part"
        failed=$((failed + 1))
        continue
    fi

    mv "$part" "$target"
    printf 'fetched %s (%s bytes; %s)\n' "$name" "$size" "${licence%%,*}"
    downloaded=$((downloaded + 1))
done < "$list"

# The SNES diagnostics arrive as one zip with a top-level `snes-test-roms/` folder; unpack it
# beside the zip so the pin probe finds `snes-test-roms/<name>.sfc` with no unzip at run time.
suite="$dest/snes-test-roms.zip"
if [ -f "$suite" ]; then
    if command -v unzip >/dev/null 2>&1; then
        if unzip -o -q "$suite" -d "$dest"; then
            printf 'unzipped snes-test-roms.zip -> %s/snes-test-roms\n' "$dest"
        else
            printf 'FAILED  snes-test-roms.zip: unzip failed\n' >&2
            failed=$((failed + 1))
        fi
    else
        printf 'SKIPPED snes-test-roms.zip: no unzip on PATH\n' >&2
    fi
fi

printf '%d fetched, %d already present, %d local-only, %d failed -> %s\n' \
    "$downloaded" "$present" "$local_only" "$failed" "$dest"
printf 'set TENDY_TEST_ROMS=%s before running the probes\n' "$dest"

[ "$failed" -eq 0 ] || exit 1
