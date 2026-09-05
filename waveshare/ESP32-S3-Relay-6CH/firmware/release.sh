#!/bin/zsh
# Publish the current firmware version as a GitHub release and point devices at it.
# Usage: ./release.sh "release notes"   (run from firmware/)
set -e
cd "$(dirname "$0")"
NOTES="${1:-}"
VER=$(grep '^version' Cargo.toml | head -1 | cut -d'"' -f2)
REPO=cascalheira/esp32-cascalheira
TAG="relay-fw-v$VER"
. ~/export-esp.sh
export RUSTUP_TOOLCHAIN=esp
cargo build --release
OUT=/tmp/relay-fw-$VER.bin
espflash save-image --chip esp32s3 --flash-size 16mb target/xtensa-esp32s3-espidf/release/relay-fw "$OUT"
gh release create "$TAG" "$OUT" --repo "$REPO" --title "relay-fw $VER" --notes "$NOTES"
SUMMARY=$(python3 -c 'import json,sys; print(json.dumps(sys.argv[1]))' "$NOTES")
cat > release/latest.json <<JSONEOF
{
  "version": "$VER",
  "url": "https://github.com/$REPO/releases/download/$TAG/relay-fw-$VER.bin",
  "summary": $SUMMARY,
  "release_url": "https://github.com/$REPO/releases/tag/$TAG"
}
JSONEOF
git add release/latest.json Cargo.toml Cargo.lock
git commit -m "Release relay-fw $VER" || true
git push
echo "released $TAG; devices will see it on their next check"
