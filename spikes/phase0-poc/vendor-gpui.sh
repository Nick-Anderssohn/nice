#!/bin/sh
# vendor-gpui.sh — (re)create vendor/gpui-0.2.2: the pristine crates.io
# extraction of gpui 0.2.2 with the Nice Phase-0 patch applied.
#
# Why: Cargo.toml patches gpui to `vendor/gpui-0.2.2` (spike 3 GPUI-side
# transactional-present toggle NICE_POC_GPUI_TXN=1 + the "Draw" os_signpost on
# subsystem dev.nickanderssohn.gpui-term). The vendored tree (~8 MB / ~185
# files) is gitignored; this script + gpui-0.2.2-nice.patch are the committed,
# reproducible source of truth.
#
# Usage: ./vendor-gpui.sh          (from spikes/phase0-poc/)
#
# Requires the extracted registry source of gpui 0.2.2 (present on any machine
# that has ever built this crate from crates.io). If it is missing, extract it
# once by building any crate that depends on gpui = "0.2.2" from the registry,
# or `tar xzf ~/.cargo/registry/cache/*/gpui-0.2.2.crate`.

set -eu

cd "$(dirname "$0")"

CARGO_HOME_DIR="${CARGO_HOME:-$HOME/.cargo}"

# Locate the pristine extraction (registry hash dir varies per machine).
SRC=""
for d in "$CARGO_HOME_DIR"/registry/src/*/gpui-0.2.2; do
    if [ -f "$d/Cargo.toml" ]; then
        SRC="$d"
        break
    fi
done

if [ -z "$SRC" ]; then
    # Fall back to extracting the .crate tarball from the download cache.
    for c in "$CARGO_HOME_DIR"/registry/cache/*/gpui-0.2.2.crate; do
        if [ -f "$c" ]; then
            echo "extracting $c"
            mkdir -p vendor
            tar xzf "$c" -C vendor
            SRC="vendor/gpui-0.2.2.pristine-tmp"
            mv vendor/gpui-0.2.2 "$SRC"
            break
        fi
    done
fi

if [ -z "$SRC" ]; then
    echo "error: pristine gpui-0.2.2 source not found under" >&2
    echo "  $CARGO_HOME_DIR/registry/{src,cache}/*/" >&2
    echo "Fetch it once (e.g. \`cargo fetch\` in a crate depending on gpui=0.2.2)" >&2
    echo "and re-run." >&2
    exit 1
fi

echo "pristine source: $SRC"
rm -rf vendor/gpui-0.2.2
mkdir -p vendor
cp -R "$SRC" vendor/gpui-0.2.2
chmod -R u+w vendor/gpui-0.2.2
rm -f vendor/gpui-0.2.2/.cargo-ok vendor/gpui-0.2.2/.cargo_vcs_info.json
rm -rf vendor/gpui-0.2.2.pristine-tmp

echo "applying gpui-0.2.2-nice.patch"
patch -p1 -s -d vendor/gpui-0.2.2 < gpui-0.2.2-nice.patch

echo "OK: vendor/gpui-0.2.2 ready (patched). \`cargo build --bin gpui-term\` away."
