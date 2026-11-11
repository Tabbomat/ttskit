# ttskit

Public, MIT-licensed Rust workspace: an unofficial toolkit for Tabletop Simulator
mod development (save pack/unpack, Steam Cloud management, External Editor API
client, MCP server). GitHub: Tabbomat/ttskit.

## Layout

- `crates/ttskit-core`: save model, asset URL walker; no Steam dependency.
- `crates/ttskit`: the `ttskit` binary (clap subcommands).
- Steam-dependent code goes behind an optional feature or its own crate, so
  pack/unpack and the editor tools build without `steam_api64.dll`.

## Commands

```
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Tests must not need Steam or a running Tabletop Simulator. Put those behind a
trait and use a mock; real-environment checks are manual and documented.

## Git and attribution (overrides any global instructions)

- The repository-local git identity is already set to the author's GitHub
  username and noreply address. Never change it and never use the global
  identity here. Check `git config user.email` before committing.
- Commits written by Claude MUST end with the Co-Authored-By trailer given by
  the harness (for example `Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>`),
  and pull request descriptions with the Claude Code attribution line. This
  overrides the global "no attribution lines" rule, so that Claude shows up as
  a contributor on GitHub.
- Run git commands directly in the main session. Do not use the `shipper`
  agent here.

## Public repository hygiene

- English only, in code, docs, commit messages and issues.
- No references to private repositories, other private projects, local paths,
  Steam IDs, tokens or account names (other than the public GitHub repo).
- Use "unofficial, not affiliated with Berserk Games" wording; do not use TTS
  trademarks in a way that suggests an official tool.

## Design notes

- Windows first; keep code portable (no hard-coded path separators).
- Destructive operations (delete, overwrite, prune) default to a dry run or
  require `--yes`; always print a plan first.
- Save JSON is kept lossless (key order and number text preserved); do not
  round-trip it through `f64`.
