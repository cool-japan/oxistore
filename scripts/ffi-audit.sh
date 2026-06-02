#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

# Explicitly enumerate forbidden C-linking crates.
# OS-binding platform crates (core-foundation-sys, security-framework-sys,
# windows-sys, windows-targets, windows_x86_64_*, windows_aarch64_*, etc.)
# are intentionally NOT listed here — they bind OS APIs without bundling C code
# and are allowed per GOVERNANCE §6 (category: "native").
# "ring v" is matched with a leading space to avoid false-positives from crate
# names that contain "ring" as a suffix (e.g. arrow-string v...).
FORBIDDEN_PATTERN='(openssl-sys|freetype-sys|harfbuzz-sys|fontconfig-sys|libsqlite3-sys|mysqlclient-sys|libz-sys|aws-lc-sys|mimalloc|simsimd|rocksdb|lmdb| ring v)'

if cargo tree --workspace --no-default-features 2>/dev/null | grep -E "${FORBIDDEN_PATTERN}"; then
    echo "FFI LEAK DETECTED in oxistore"
    exit 1
fi
echo "oxistore FFI audit: CLEAN"
