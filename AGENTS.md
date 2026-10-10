# Repository Guidelines

Hero Siege Codex: a zero-dependency Node.js toolkit (valuation engine, CLI, HTTP
API, SPA) for Hero Siege item pricing and loot triage, plus a Discord
social-signal pipeline and a vendored Rust packet-sniffer (`hs-sniff/`) with a
live market client.

## Start here

- `docs/architecture.md` — module layout, data flow, commands, important files.
- `docs/conventions.md` — purity rules, valuation model, query language, naming,
  testing. The review agent reads this.
- `hs-sniff/README.md` — capture, CLI, MCP tools, market client, troubleshooting.
- `IDEAS.md` — backlog; check before proposing a new direction.

## Non-negotiables

- **The Node app stays dependency-free.** Only `node:*` built-ins in `src/` and
  `bin/`. No bundler, transpiler, or third-party Node runtime dep. (`hs-sniff` is
  Rust; the zero-dep rule does not apply to it.)
- **Purity holds at the seams.** `engine.js` and `social.js` are pure — no fs, no
  network. I/O lives in `server.js`, `social-source.js`, `bin/`, and
  `hs-sniff/src/market.rs`'s `fetch_market`.
- **Derived facts only** in the social corpus: authors are short hashes, never
  names or ids.
- Never commit a Discord session token or `.env`.

## Reference implementations

`refs/` holds read-only reference projects (gitignored; `hs-tracker` is a
submodule). The market contract lives in
`refs/Hero-Siege-Companion/docs/market.md` and `src/main/market-*.ts`; RE notes
in `refs/hero-siege/INTERNALS.md` and `SECURITY-REVIEW.md`. Check here before
re-deriving protocol behaviour — it has usually already been done.

## Checks

`scripts/check.sh` runs the Node suites plus `hs-sniff`'s `cargo test`. It is the
pre-commit hook (`.githooks/`, enabled with `git config core.hooksPath .githooks`);
`npm run guardrail` runs the same thing by hand. There is no CI — this is a
private repo without paid runners, so the hook is the guardrail.

`npm test` alone covers the Node suites only; `refs/`, `hs-plan/`, and
`hs-sniff/` are excluded from it by design.

## Runtime

Node.js >= 18 (native `node:test`, ESM), npm, compatible with Bun. `hs-sniff`
builds with cargo.
