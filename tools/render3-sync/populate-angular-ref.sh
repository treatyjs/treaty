#!/usr/bin/env bash
# Populate tools/angular-ref with Angular's packages/compiler sources and the compliance corpus at
# the commit pinned in tools/render3-sync/ANGULAR_REF (or at --sha). Uses a blobless, depth-1,
# sparse git checkout so only the two directories the harness reads are downloaded.
#
# Usage:
#   tools/render3-sync/populate-angular-ref.sh [--sha <40-hex>] [--dest <dir>]
#
#   --sha   Angular commit to check out (default: contents of tools/render3-sync/ANGULAR_REF).
#   --dest  Directory to populate (default: <repo>/tools/angular-ref).
#
# Environment:
#   ANGULAR_REMOTE  Git remote to fetch from (default: https://github.com/angular/angular.git).
#                   Useful for tests against a local clone.
#
# The script refuses to touch a symlink or a non-empty directory that is not already a checkout of
# this script's remote, so the locally provided tools/angular-ref link is never overwritten.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
SHA_FILE="$SCRIPT_DIR/ANGULAR_REF"
REMOTE="${ANGULAR_REMOTE:-https://github.com/angular/angular.git}"
SPARSE_PATHS=(packages/compiler packages/compiler-cli/test/compliance)

usage() {
  sed -n '2,15p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
}

die() {
  echo "populate-angular-ref: $*" >&2
  exit 1
}

sha=""
dest="$REPO_ROOT/tools/angular-ref"
while [[ $# -gt 0 ]]; do
  case "$1" in
    --sha) [[ $# -ge 2 ]] || die "--sha requires a value"; sha="$2"; shift 2 ;;
    --dest) [[ $# -ge 2 ]] || die "--dest requires a value"; dest="$2"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) die "unknown argument: $1 (see --help)" ;;
  esac
done

if [[ -z "$sha" ]]; then
  [[ -f "$SHA_FILE" ]] || die "no --sha given and $SHA_FILE is missing"
  sha="$(tr -d '[:space:]' < "$SHA_FILE")"
fi
[[ "$sha" =~ ^[0-9a-f]{40}$ ]] || die "not a full 40-character lowercase commit SHA: '$sha'"

if [[ -L "$dest" ]]; then
  die "$dest is a symlink; refusing to populate through it (remove the link first if you mean to replace it)"
fi
if [[ -e "$dest" && ! -d "$dest" ]]; then
  die "$dest exists and is not a directory"
fi
if [[ -d "$dest" && -n "$(ls -A "$dest")" ]]; then
  if [[ ! -d "$dest/.git" ]]; then
    die "$dest is non-empty and not a git checkout; refusing to overwrite it"
  fi
  existing_remote="$(git -C "$dest" remote get-url origin 2>/dev/null || true)"
  [[ "$existing_remote" == "$REMOTE" ]] || die "$dest is a checkout of '$existing_remote', not '$REMOTE'"
fi

mkdir -p "$dest"
cd "$dest"
if [[ ! -d .git ]]; then
  git init -q .
fi
if git remote get-url origin >/dev/null 2>&1; then
  git remote set-url origin "$REMOTE"
else
  git remote add origin "$REMOTE"
fi

git sparse-checkout set --cone "${SPARSE_PATHS[@]}" >/dev/null
git -c protocol.version=2 fetch --quiet --depth 1 --filter=blob:none origin "$sha" \
  || die "could not fetch $sha from $REMOTE"
git checkout --quiet --detach "$sha" || die "checkout of $sha failed"

head="$(git rev-parse HEAD)"
[[ "$head" == "$sha" ]] || die "checked out $head, expected $sha"
[[ -d packages/compiler/src ]] || die "packages/compiler/src missing after checkout"

ts_count="$(find packages/compiler/src -name '*.ts' | wc -l | tr -d ' ')"
echo "populate-angular-ref: $dest is at $sha ($ts_count compiler .ts files)"
