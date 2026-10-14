#!/bin/sh
# Wrap a built helper in LogosQR.app and sign it with the hardened runtime and the camera entitlement.
#   make-app.sh <logos-qr-helper binary> <out dir> <version> [--release]
# Without --release the bundle id ends in .dev, so a local build never touches the release's grant.
# CODESIGN_IDENTITY (default "-", ad hoc) names a Developer ID for a release.
set -eu

bin=$1
out=$2
version=$3
release=${4:-}
here=$(cd "$(dirname "$0")/.." && pwd)

if [ "$release" = "--release" ]; then
    id=co.logos.QRHelper
    name="Logos QR"
else
    id=co.logos.QRHelper.dev
    name="Logos QR (dev)"
fi

app="$out/LogosQR.app"
rm -rf "$app"
mkdir -p "$app/Contents/MacOS"
cp "$bin" "$app/Contents/MacOS/logos-qr-helper"
chmod 755 "$app/Contents/MacOS/logos-qr-helper"
sed -e "s|@BUNDLE_ID@|$id|g" -e "s|@NAME@|$name|g" -e "s|@VERSION@|$version|g" \
    "$here/macos/Info.plist.in" > "$app/Contents/Info.plist"
printf 'APPL????' > "$app/Contents/PkgInfo"

identity=${CODESIGN_IDENTITY:--}
if [ "$identity" = "-" ]; then
    codesign --force --sign - --options runtime --entitlements "$here/macos/LogosQR.entitlements" "$app"
else
    codesign --force --sign "$identity" --timestamp --options runtime \
        --entitlements "$here/macos/LogosQR.entitlements" "$app"
fi
codesign --verify --strict --verbose=2 "$app"
echo "$app"
