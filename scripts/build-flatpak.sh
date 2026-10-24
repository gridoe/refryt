#!/usr/bin/env bash
# Build the Flatpak bundle: dist/refryt-<version>-x86_64.flatpak.
#
# Needs flatpak and flatpak-builder (org.flatpak.Builder from Flathub works), the GNOME 51
# Platform and Sdk, and org.freedesktop.Sdk.Extension.rust-stable//26.08. Needs the network.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"
dir="packaging/flatpak"
id="io.github.gridoe.refryt"

version="$(cargo metadata --no-deps --format-version 1 \
    | python3 -c 'import json, sys; print(next(p["version"] for p in json.load(sys.stdin)["packages"] if p["name"] == "refryt"))')"
out="dist/refryt-$version-x86_64.flatpak"

if command -v flatpak-builder > /dev/null; then
    builder=(flatpak-builder)
else
    builder=(flatpak run --command=flatpak-builder org.flatpak.Builder)
fi

# Kept out of git: flathub's shared recipes (Lua), and the newest yt-dlp, fetched
# fresh like the AppImage's — an old pinned copy would defeat the point of having it.
#
# The recipes are pinned, unlike yt-dlp: they are other people's build instructions, and a build
# here and one in CI should use the same files. Move the pin on purpose, after a build with the
# new revision works.
shared_modules_rev=cb9ec602a1ece1c76d5a4f8aa1d87c4a6bf99c3e
if [ "$(git -C "$dir/shared-modules" rev-parse HEAD 2> /dev/null)" != "$shared_modules_rev" ]; then
    rm -rf "$dir/shared-modules"
    git init -q "$dir/shared-modules"
    git -C "$dir/shared-modules" fetch -q --depth 1 \
        https://github.com/flathub/shared-modules.git "$shared_modules_rev"
    git -C "$dir/shared-modules" checkout -q FETCH_HEAD
fi
echo "fetching yt-dlp"
curl -fsSL -o "$dir/yt-dlp" \
    https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp

# With the signing key (CI imports it with scripts/import-flatpak-key.sh), the commit is signed
# and the bundle names the hosted repo (.github/workflows/flatpak-repo.yml) and its key, so an
# install from the file gets its updates from there with `flatpak update`. Flatpak refuses such
# a bundle unless its commit is signed, so without the key the bundle is a plain one, as before.
sign_args=()
bundle_args=()
if [ -n "${FLATPAK_GPG_KEY_ID:-}" ]; then
    sign_args=(--gpg-sign="$FLATPAK_GPG_KEY_ID" --gpg-homedir="$FLATPAK_GPG_HOMEDIR")
    bundle_args=(--repo-url=https://gridoe.github.io/refryt/repo
        --runtime-repo=https://dl.flathub.org/repo/flathub.flatpakrepo
        --gpg-keys="$dir/refryt.gpg")
fi

mkdir -p dist
"${builder[@]}" --force-clean --user --disable-rofiles-fuse "${sign_args[@]}" \
    --state-dir="$dir/.flatpak-builder" \
    --repo="$dir/repo" "$dir/build-dir" "$dir/$id.yml"
flatpak build-bundle "${bundle_args[@]}" "$dir/repo" "$out" "$id"
echo "built $out ($(du -h "$out" | cut -f1))"
