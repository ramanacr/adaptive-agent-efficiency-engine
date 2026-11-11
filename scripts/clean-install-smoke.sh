#!/usr/bin/env bash
# Clean-install smoke test of one release archive, on a fresh CI runner.
#
#   scripts/clean-install-smoke.sh <dist-dir> <target>
#
# <target> is the archive suffix: x86_64-pc-windows-msvc, aarch64-pc-windows-msvc,
# x86_64-unknown-linux-musl, aarch64-unknown-linux-musl or universal-apple-darwin.
#
# Checks, with a fresh home folder and no Rust toolchain needed:
#   1. the archive matches SHA256SUMS;
#   2. the binary has no dynamic C-runtime dependency (Windows: no VCRUNTIME140.dll;
#      Linux: static; macOS: system libraries only);
#   3. `aaes --version` prints the archive's version (macOS: also the x86_64 slice, via Rosetta);
#   4. `aaes init --mode command` writes hooks into a temporary settings file;
#   5. a hook call (command mode) shapes a large Grep result, and `aaes show` returns the
#      lines it left out from the encrypted store;
#   6. `aaes report --json` counts the event, and `aaes uninstall` removes the hooks.
set -euo pipefail

dist=${1:?dist dir}
target=${2:?target}
fail() { echo "FAIL: $*" >&2; exit 1; }
step() { echo "--- $*"; }

archive=$(cd "$dist" && ls aaes-v*-"$target".* 2>/dev/null | head -n1)
[ -n "$archive" ] || fail "no archive for $target in $dist"
version=${archive#aaes-v}
version=${version%-"$target".*}
echo "archive: $archive (version $version)"

step "1. checksum"
want=$(grep " \*\?$archive\$" "$dist/SHA256SUMS" | cut -d' ' -f1)
[ -n "$want" ] || fail "$archive not in SHA256SUMS"
if command -v sha256sum >/dev/null; then
  got=$(sha256sum "$dist/$archive" | cut -d' ' -f1)
else
  got=$(shasum -a 256 "$dist/$archive" | cut -d' ' -f1)
fi
[ "$want" = "$got" ] || fail "checksum mismatch: $got != $want"

work=$(mktemp -d)
case "$archive" in
  *.zip)
    if command -v unzip >/dev/null; then
      unzip -q "$dist/$archive" -d "$work/pkg"
    else
      powershell -NoProfile -Command "Expand-Archive -Path '$dist/$archive' -DestinationPath '$work/pkg'"
    fi
    aaes="$work/pkg/aaes.exe" ;;
  *.tar.gz)
    mkdir -p "$work/pkg" && tar -xzf "$dist/$archive" -C "$work/pkg"
    aaes="$work/pkg/aaes" ;;
  *) fail "unknown archive type: $archive" ;;
esac
[ -f "$aaes" ] || fail "no binary in archive"
for f in LICENSE README.md; do [ -f "$work/pkg/$f" ] || fail "$f missing from archive"; done

step "2. runtime dependencies"
case "$target" in
  *windows*)
    if grep -aqi 'vcruntime140' "$aaes"; then fail "binary references VCRUNTIME140.dll (needs the VC++ Redistributable)"; fi ;;
  *linux*)
    desc=$(file "$aaes")
    echo "$desc"
    case "$desc" in *"statically linked"*|*"static-pie linked"*) ;; *) fail "not statically linked" ;; esac ;;
  *apple*)
    otool -L "$aaes"
    # Dependency lines are indented; a universal binary has one unindented header per slice.
    deps=$(otool -L "$aaes" | awk '/^[ \t]/ { print $1 }')
    bad=$(printf '%s\n' "$deps" | grep -v -e '^/usr/lib/' -e '^/System/Library/' -e '^$' || true)
    [ -n "$deps" ] || fail "otool listed no dependencies"
    [ -z "$bad" ] || fail "links a non-system library: $(printf '%s' "$bad" | od -c | head -n 5)"
    lipo -info "$aaes" ;;
esac

step "3. version"
out=$("$aaes" --version)
[ "$out" = "aaes $version" ] || fail "--version printed '$out', expected 'aaes $version'"
if [ "$(uname -s)" = Darwin ] && [ "$(uname -m)" = arm64 ]; then
  if arch -x86_64 /usr/bin/true 2>/dev/null; then
    out=$(arch -x86_64 "$aaes" --version)
    [ "$out" = "aaes $version" ] || fail "x86_64 slice printed '$out'"
    echo "x86_64 slice (Rosetta): ok"
  else
    echo "::warning::Rosetta not available; x86_64 slice not run"
  fi
fi

# Fresh, empty user folders: nothing from the runner's profile leaks in.
export AAES_HOME="$work/aaes-home"
export CLAUDE_CONFIG_DIR="$work/claude"
export HOME="$work/home" USERPROFILE="$work/home"
mkdir -p "$HOME" "$CLAUDE_CONFIG_DIR"
settings="$work/claude/settings.json"

step "4. init (command mode)"
"$aaes" init --mode command --settings "$settings"
grep -q 'PostToolUse' "$settings" || fail "no PostToolUse hook in settings"
grep -q 'aaes' "$settings" || fail "hook command does not name aaes"

step "5. hook call shapes a large Grep result; show returns omitted lines"
content=$(awk 'BEGIN { for (f = 0; f < 30; f++) for (l = 1; l <= 40; l++) printf "src/module%d/file%d.ts:%d:  const value%d = compute(input, %d);\\n", f, f, l, l, l }')
payload='{"hook_event_name":"PostToolUse","session_id":"clean-install","cwd":"/repo","tool_name":"Grep","tool_input":{"pattern":"compute","output_mode":"content"},"tool_response":{"mode":"content","numFiles":0,"filenames":[],"content":"'"$content"'","numLines":1200}}'
resp=$(printf '%s' "$payload" | "$aaes" hook)
case "$resp" in *updatedToolOutput*) ;; *) fail "hook did not shape the result: ${resp:0:300}" ;; esac
case "$resp" in *"1200 matches in 30 files"*) ;; *) fail "exact totals missing from the shaped result" ;; esac
handle=$(printf '%s' "$resp" | grep -o 'aaes show h_[a-z0-9]*' | head -n1 | cut -d' ' -f3)
[ -n "$handle" ] || fail "no handle in the shaped result"
shown=$("$aaes" show "$handle" --grep 'module29/file29.ts:40:')
case "$shown" in *"value40 = compute(input, 40)"*) ;; *) fail "aaes show did not return the omitted line" ;; esac

step "6. report and uninstall"
report=$("$aaes" report --json)
case "$report" in *'"sessions":1'*) ;; *) fail "report does not count the session: ${report:0:300}" ;; esac
case "$report" in *'"search"'*) ;; *) fail "report has no search row" ;; esac
"$aaes" uninstall --settings "$settings"
if grep -q 'aaes' "$settings"; then fail "hooks still in settings after uninstall"; fi
if [ -n "$("$aaes" logs --errors --all 2>/dev/null | grep -v 'client closed early' || true)" ]; then
  "$aaes" logs --errors --all
  fail "errors or warnings in the AAES log"
fi

echo "PASS: clean install of $archive"
