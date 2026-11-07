#!/bin/sh
# Install or update AAES on Linux or macOS from a GitHub Release. No root needed.
#
#   curl -fsSL https://raw.githubusercontent.com/ramanacr/adaptive-agent-efficiency-engine/main/scripts/install.sh | sh
#
# Environment:
#   AAES_VERSION      release version without "v" (default: latest published release)
#   AAES_INSTALL_DIR  install folder (default: ~/.local/bin)
#   AAES_SOURCE       local folder with the archive and SHA256SUMS (for testing)
#
# Checks the archive against the release's SHA256SUMS. If the GitHub CLI (gh) is available,
# it also verifies the Sigstore build provenance. Then run `aaes init`.
set -eu

repo="ramanacr/adaptive-agent-efficiency-engine"
dir="${AAES_INSTALL_DIR:-$HOME/.local/bin}"
src="${AAES_SOURCE:-}"

case "$(uname -s)" in
  Linux)
    case "$(uname -m)" in
      x86_64|amd64) target="x86_64-unknown-linux-musl" ;;
      aarch64|arm64) target="aarch64-unknown-linux-musl" ;;
      *) echo "Unsupported architecture: $(uname -m)" >&2; exit 1 ;;
    esac ;;
  Darwin) target="universal-apple-darwin" ;;
  *) echo "Unsupported OS: $(uname -s); on Windows use install.ps1" >&2; exit 1 ;;
esac

version="${AAES_VERSION:-}"
if [ -z "$version" ]; then
  if [ -n "$src" ]; then
    f=$(ls "$src"/aaes-v*-"$target".tar.gz 2>/dev/null | head -n1)
    [ -n "$f" ] || { echo "No aaes-v*-$target.tar.gz in $src" >&2; exit 1; }
    version=$(basename "$f" | sed -e 's/^aaes-v//' -e "s/-$target.tar.gz\$//")
  else
    version=$(curl -fsSL -H 'User-Agent: aaes-installer' "https://api.github.com/repos/$repo/releases/latest" \
      | sed -n 's/.*"tag_name": *"v\{0,1\}\([^"]*\)".*/\1/p' | head -n1)
    [ -n "$version" ] || { echo "Could not find the latest release" >&2; exit 1; }
  fi
fi

archive="aaes-v$version-$target.tar.gz"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

for name in "$archive" SHA256SUMS; do
  if [ -n "$src" ]; then cp "$src/$name" "$tmp/$name"
  else curl -fsSL -o "$tmp/$name" "https://github.com/$repo/releases/download/v$version/$name"; fi
done

expected=$(grep " \*\{0,1\}$archive\$" "$tmp/SHA256SUMS" | awk '{print $1}' | head -n1)
[ -n "$expected" ] || { echo "$archive is not listed in SHA256SUMS" >&2; exit 1; }
if command -v sha256sum >/dev/null 2>&1; then actual=$(sha256sum "$tmp/$archive" | awk '{print $1}')
else actual=$(shasum -a 256 "$tmp/$archive" | awk '{print $1}'); fi
[ "$expected" = "$actual" ] || { echo "Checksum mismatch for $archive" >&2; exit 1; }
echo "Checksum OK: $archive"

if [ -z "$src" ] && command -v gh >/dev/null 2>&1; then
  if gh auth status >/dev/null 2>&1; then
    gh attestation verify "$tmp/$archive" --repo "$repo" >/dev/null 2>&1 \
      || { echo "Build provenance check failed for $archive (gh attestation verify)" >&2; exit 1; }
    echo "Build provenance verified (Sigstore)."
  else
    echo "Skipped the build provenance check: gh is not signed in (gh auth login)."
  fi
fi

mkdir -p "$dir"
[ -x "$dir/aaes" ] && "$dir/aaes" serve --stop >/dev/null 2>&1 || true
tar -xzf "$tmp/$archive" -C "$tmp" aaes
install -m 0755 "$tmp/aaes" "$dir/aaes"
echo "Installed $("$dir/aaes" --version) to $dir/aaes"

case ":$PATH:" in
  *":$dir:"*) ;;
  *) echo "Add $dir to your PATH, for example: echo 'export PATH=\"$dir:\$PATH\"' >> ~/.profile" ;;
esac
echo "Next: run 'aaes init' to add the Claude Code hooks."
