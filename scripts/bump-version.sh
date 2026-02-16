#!/usr/bin/env bash
set -euo pipefail

# Reads the workspace version from the root Cargo.toml and propagates it
# to every package in the repository.
#
# Rust crates:  [workspace.package] + [workspace.dependencies] in root
#               Cargo.toml — sub-crates inherit via `workspace = true`.
# Node.js:     optionalDependencies use pnpm `workspace:*` protocol —
#               only the platform packages and main package.json version
#               need updating. pnpm resolves the rest at publish time.
#
# Usage:
#   1. Edit Cargo.toml  →  [workspace.package] version = "x.y.z"
#   2. Run ./scripts/bump-version.sh
#   3. Commit

ROOT="$(cd "$(dirname "$0")/.." && pwd)"

VERSION=$(perl -0777 -ne 'print $1 if /\[workspace\.package\].*?^version\s*=\s*"([^"]+)"/ms' "$ROOT/Cargo.toml")

if [ -z "$VERSION" ]; then
  echo "ERROR: could not read workspace version from Cargo.toml" >&2
  exit 1
fi

echo "Bumping all packages to $VERSION"

# --- Rust workspace dependency versions (root Cargo.toml only) ---
# Enumerate publishable crates via cargo metadata (publish != false)
CRATES=$(cargo metadata --no-deps --format-version 1 \
  | yq -r '.packages[] | select(.source == null and .publish == null) | .name')

for crate in $CRATES; do
  perl -pi -e "s/($crate\s*=\s*\{\s*version\s*=\s*)\"[^\"]+\"/\1\"$VERSION\"/" "$ROOT/Cargo.toml"
done

# --- Python ---
perl -pi -e "s/^(version\s*=\s*)\"[^\"]+\"/\1\"$VERSION\"/" "$ROOT/sayiir-python/pyproject.toml"
perl -pi -e "s/^(__version__\s*=\s*)\"[^\"]+\"/\1\"$VERSION\"/" "$ROOT/sayiir-python/sayiir/__init__.py"

# --- Node.js ---
yq -i -oj -I2 ".version = \"$VERSION\"" "$ROOT/sayiir-nodejs/package.json"
for pkg in "$ROOT"/sayiir-nodejs/npm/*/package.json; do
  yq -i -oj -I2 ".version = \"$VERSION\"" "$pkg"
done

# --- Cargo.lock ---
cd "$ROOT"
cargo generate-lockfile 2>/dev/null

echo "Done — all packages now at $VERSION"
