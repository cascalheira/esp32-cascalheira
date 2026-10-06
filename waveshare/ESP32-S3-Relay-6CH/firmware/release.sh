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
# rustup proxies must win over a Homebrew rust, or the Xtensa toolchain is never used.
export PATH="$HOME/.cargo/bin:$PATH"
export RUSTUP_TOOLCHAIN=esp
# Public image: never embed the WiFi/API-key seed from secrets.env.
RELAY_NO_SEED=1 cargo build --release
OUT=/tmp/relay-fw-$VER.bin
espflash save-image --chip esp32s3 --flash-size 16mb target/xtensa-esp32s3-espidf/release/relay-fw "$OUT"
# Refuse to publish if any value from any seed file made it into the image.
for f in secrets*.env(N); do
  sed -nE 's/^(WIFI_PASS|NOISE_PSK|WIFI_SSID)=//p' "$f" | while IFS= read -r v; do
    if [ -n "$v" ] && grep -q -a -F -- "$v" "$OUT"; then echo "ABORT: a secret from $f is inside $OUT"; exit 1; fi
  done || exit 1
done
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
