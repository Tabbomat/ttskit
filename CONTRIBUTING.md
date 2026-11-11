# Contributing to ttskit

ttskit is an unofficial toolkit and is not affiliated with Berserk Games.
Contributions are welcome; please keep code, docs, commit messages and issues
in English.

## Checks

Run these before sending a change:

```
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Tests must not need Steam or a running Tabletop Simulator. Code that talks to
either goes behind a trait so the tests can use a mock.

## Test fixtures

### Policy

Sample saves under `tests/fixtures/` must be **synthetic** (written by hand
for the test) or **your own work** that you are free to publish under the
project's MIT license.

Never commit third-party mod content: no Workshop saves, no excerpts of other
people's scripts or UI, and no asset URLs taken from their mods. Asset URLs in
fixtures use obviously fake values such as `https://example.com/...` or
Steam-cloud-shaped URLs with zeroed IDs.

To test against real mods, use the local-only mode described below instead of
committing them.

### Format

Fixtures are stored exactly as Tabletop Simulator writes saves: two-space
indentation, CRLF line endings, no trailing newline and no byte order mark.
`.gitattributes` marks `tests/fixtures/**` as `-text` so git never converts the
line endings. A fixture must round-trip byte for byte through
`ttskit_core::Save`; the `fixtures` test in `ttskit-core` checks every
`*.json` file in the directory automatically.

To add a fixture, write the JSON by hand, normalize it once by reading it with
`Save::from_slice` and writing `Save::to_vec` back to the file, and check the
diff to confirm that only whitespace changed.

### Locating fixtures from tests

The unpublished `ttskit-testutil` crate finds fixtures from the tests of any
crate in the workspace. Add it as a dev-dependency:

```toml
[dev-dependencies]
ttskit-testutil = { path = "../ttskit-testutil" }
```

and use `ttskit_testutil::read_fixture("minimal_save.json")`,
`fixture_path`, `fixtures_dir` or `fixture_saves`.

## Checking real saves locally

The `real_saves` test in `ttskit-core` can round-trip a directory of real saves
that are not committed. Point `TTSKIT_REAL_SAVES` at the directory; every
`*.json` file below it is read (never written) and checked:

```
TTSKIT_REAL_SAVES=/path/to/saves cargo test -p ttskit-core --test real_saves -- --nocapture
```

In PowerShell:

```
$env:TTSKIT_REAL_SAVES = 'C:\path\to\saves'
cargo test -p ttskit-core --test real_saves -- --nocapture
Remove-Item Env:TTSKIT_REAL_SAVES
```

- Without the variable the test returns early, so it is skipped in CI and in
  a plain `cargo test`.
- If the variable names something that is not a directory, the test fails
  instead of passing silently.
- Files written by Tabletop Simulator (CRLF, no trailing newline) must
  round-trip byte for byte. Other JSON files are only checked structurally:
  the output must re-parse to the same keys, order and values, and writing it
  again must give the same bytes.
- The summary line lists how many files were byte-exact, structural-only and
  failed; failures show the first differing byte with some context.
