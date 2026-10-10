#!/bin/sh
# Repo guardrail: the Node suites plus the hs-sniff Rust crate.
#
#   sh scripts/check.sh
#
# Wired as a pre-commit hook through .githooks (see AGENTS.md). Deliberately
# not a CI job: this is a private repo and GitHub runners are not paid for.
set -e
cd "$(git rev-parse --show-toplevel 2>/dev/null || echo .)"

echo "== node suites =="
npm test

if command -v cargo >/dev/null 2>&1; then
  echo "== hs-sniff (cargo test) =="
  cargo test --manifest-path hs-sniff/Cargo.toml
else
  echo "! cargo not found — skipping the hs-sniff suite"
fi

echo "checks passed"
