#!/usr/bin/env bash
# Import the Flatpak signing key for CI: the armoured private key in $FLATPAK_GPG_KEY, whose
# public half is packaging/flatpak/refryt.gpg. Prints the two lines a later step needs,
# for $GITHUB_ENV:
#
#   FLATPAK_GPG_HOMEDIR=<a new keyring>
#   FLATPAK_GPG_KEY_ID=<its fingerprint>
#
# A key that is not the committed one fails here, rather than signing a repo or bundle that
# installs will not accept.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
[ -n "${FLATPAK_GPG_KEY:-}" ] || { echo "FLATPAK_GPG_KEY is not set" >&2; exit 1; }

home="$(mktemp -d)"
chmod 700 "$home"
printf '%s\n' "$FLATPAK_GPG_KEY" | gpg --homedir "$home" --batch --quiet --import
key="$(gpg --homedir "$home" --list-secret-keys --with-colons | awk -F: '/^fpr/ { print $10; exit }')"
public="$(gpg --homedir "$home" --with-colons --import-options show-only \
    --import "$root/packaging/flatpak/refryt.gpg" | awk -F: '/^fpr/ { print $10; exit }')"
[ "$key" = "$public" ] || { echo "the key is not packaging/flatpak/refryt.gpg" >&2; exit 1; }

echo "FLATPAK_GPG_HOMEDIR=$home"
echo "FLATPAK_GPG_KEY_ID=$key"
