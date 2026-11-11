# ttskit

An unofficial command line toolkit (and MCP server) for developing
[Tabletop Simulator](https://store.steampowered.com/app/286160/Tabletop_Simulator/)
mods. Not affiliated with or endorsed by Berserk Games.

> **Status:** early development. Only `cloud list` works so far; see the
> issue tracker for the roadmap.

## Planned features

- **Save pack/unpack:** split a save into readable, diff-friendly files and
  assemble it again, byte-exact, with `require` / `<Include/>` bundling.
- **Steam Cloud management:** one cloud folder per game, sync with a local
  directory, a committed manifest of asset URLs, and a scan of the asset URLs
  a save uses.
- **External Editor API client:** talk to a running game to read and write
  scripts, run Lua and capture console output.
- **MCP server:** expose the editor API to AI assistants.

## Building

```
cargo build
cargo test
```

Windows is the primary platform at first.

See [CONTRIBUTING](CONTRIBUTING.md) for the checks to run, the test fixture
policy and how to test against real saves locally.

## Steam Cloud

The `cloud` commands talk to the running Steam client and need a build with
the `steam` feature:

```
cargo build --release --features steam
ttskit cloud list
ttskit cloud list --json
```

`cloud list` prints the files in your Tabletop Simulator Steam Cloud, with
their sizes and the total. It only reads. Folders (as the game shows them) are
not listed yet.

Requirements:

- Steam is running and logged in, as an account that owns Tabletop Simulator.
- ttskit runs as the same Windows user (and elevation level) as Steam.
- Steam Cloud is enabled for the account and for the game.
- `steam_api64.dll` is next to `ttskit.exe`. `cargo run` and `cargo test`
  find it on their own; to run the built executable directly, copy the DLL
  from the Steamworks SDK (`redistributable_bin/win64`) next to it.

Without the `steam` feature, ttskit builds and runs without the Steamworks
SDK, and the `cloud` commands exit with an error.

### Known limitations

- While connecting, the Steam library prints two diagnostic lines to stderr.
  One of them contains your Steam ID. ttskit does not control this output. To
  keep it out of logs, redirect stderr (`2>NUL` in cmd, `2>$null` in
  PowerShell); errors from ttskit itself also go to stderr.
- ttskit connects to Steam as Tabletop Simulator (app id 286160), so Steam
  may show you as playing the game while a `cloud` command runs.

### Can the game stay open?

Yes, for reading. `cloud list` gave the same result with the game closed and
with the game running at its main menu, and the game kept running. Commands
that change the cloud do not exist yet; whether they are safe while the game
runs will be checked when they are added.

### steam_api64.dll and releases

`steam_api64.dll` is part of Valve's Steamworks SDK, not of ttskit, and is not
covered by the MIT license. This repository does not contain it; builds get it
from the [`steamworks-sys`](https://crates.io/crates/steamworks-sys) crate,
which bundles the SDK's redistributable files. The
[Steamworks SDK Access Agreement](https://partner.steamgames.com/documentation/sdk_access_agreement)
(section 1.1) allows distributing the files in the SDK's `redistributable_bin`
folder together with the software that uses them.

Releases will therefore ship two downloads:

- a plain build without Steam support, which needs no DLL;
- a Steam build: a zip with `ttskit.exe` and `steam_api64.dll` side by side.

## License

[MIT](LICENSE)
