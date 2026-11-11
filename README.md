# ttskit

An unofficial command line toolkit (and MCP server) for developing
[Tabletop Simulator](https://store.steampowered.com/app/286160/Tabletop_Simulator/)
mods. Not affiliated with or endorsed by Berserk Games.

> **Status:** early development. Only `cloud list` and `cloud restore` work
> so far; see the issue tracker for the roadmap.

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
ttskit cloud list --folder Decks/Old
```

`cloud list` prints the folders and files of your Tabletop Simulator Steam
Cloud as a tree, the way the game's Cloud Manager shows them, with sizes and
totals. It only reads.

```
Decks/
  Old/
  back.png    2048
  front.png  12345
Tokens/
  Red/
    red.png    100  missing
readme.txt      10
Not in index:
  stray.png    512
5 files in 4 folders, 15015 bytes, 1 missing, 1 not in index
```

- `missing`: the game's index lists the file, but the file is not in the
  cloud.
- `Not in index:` files in the cloud that the game's index does not list, so
  the game does not show them. Files the game uses internally (the index
  files, `WorkshopUpload`, `WorkshopImageUpload.png`) are left out.
- `--folder F` shows only folder `F` and its subfolders. Folders are separated
  by `/` and matched case-insensitively.
- `--json` prints every folder and file with its cloud file name, download URL,
  date and status (`ok`, `missing` or `unindexed`).

The folder tree comes from two index files the game keeps in the cloud,
`CloudInfo.bson` (one entry per file: name, URL, size, date, folder) and
`CloudFolder.bson` (the folder list). Cloud files are named
`<SHA1>_<name>`, where the SHA1 is of the file content. ttskit keeps fields it
does not know when it reads the index. The format follows the MIT-licensed
community tool [TTSCloudSync](https://github.com/Chatanga/TTSCloudSync).

Requirements:

- Steam is running and logged in, as an account that owns Tabletop Simulator.
- ttskit runs as the same Windows user (and elevation level) as Steam.
- Steam Cloud is enabled for the account and for the game.
- `steam_api64.dll` is next to `ttskit.exe`. `cargo run` and `cargo test`
  find it on their own; to run the built executable directly, copy the DLL
  from the Steamworks SDK (`redistributable_bin/win64`) next to it.

Without the `steam` feature, ttskit builds and runs without the Steamworks
SDK, and the `cloud` commands that talk to Steam exit with an error.

### Index backups and restore

Before ttskit changes the cloud index, it saves a copy of both index files
to a local backup folder named after the UTC time, for example
`20261111T184948Z`. By default backups live in
`%LOCALAPPDATA%\ttskit\cloud-backups` (the local data directory on other
systems). `--backup-dir DIR` or the `TTSKIT_BACKUP_DIR` environment variable
picks another directory. ttskit never deletes backups.

```
ttskit cloud restore                         # list backups, newest first
ttskit cloud restore 20261111T184948Z        # show what a restore would change
ttskit cloud restore 20261111T184948Z --yes  # restore it
```

Without `--yes`, `cloud restore` only prints the plan: per index file
`write`, `delete` (the file did not exist when the backup was taken) or
`unchanged`. With `--yes` it first backs up the current index, so a restore
can itself be undone. A backup whose files are not a valid index is refused.
Listing backups does not need Steam.

Close the game before restoring: whether the game notices index changes
made while it runs, or overwrites them, has not been checked yet.

### Known limitations

- While connecting, the Steam library prints two diagnostic lines to stderr.
  One of them contains your Steam ID. ttskit does not control this output. To
  keep it out of logs, redirect stderr (`2>NUL` in cmd, `2>$null` in
  PowerShell); errors from ttskit itself also go to stderr.
- ttskit connects to Steam as Tabletop Simulator (app id 286160), so Steam
  may show you as playing the game while a `cloud` command runs.

### Can the game stay open?

Yes, for reading. `cloud list` gave the same result with the game closed and
with the game running at its main menu, and the game kept running. For
commands that change the cloud (`cloud restore --yes`) this has not been
checked yet, so close the game first.

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
