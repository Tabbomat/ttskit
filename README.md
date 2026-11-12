# ttskit

An unofficial command line toolkit (and MCP server) for developing
[Tabletop Simulator](https://store.steampowered.com/app/286160/Tabletop_Simulator/)
mods. Not affiliated with or endorsed by Berserk Games.

> **Status:** early development. Only the `cloud` commands (`list`,
> `upload`, `url`, `delete`, `status` and `restore`) work so far; see the
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
- `--folder F` shows only folder `F` and its subfolders. Folders are matched
  case-insensitively, and levels may be separated by `/` or `\`. The game
  stores `\`; ttskit shows `/`, also in the JSON output.
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

### Upload and URLs

```
ttskit cloud upload deck.png --folder Decks/Old --dry-run  # show the plan
ttskit cloud upload deck.png --folder Decks/Old            # asks before changing anything
ttskit cloud upload deck.png --folder Decks/Old --yes      # no question
ttskit cloud url Decks/Old/deck.png
```

`cloud upload FILE` writes the file to the cloud, shares it, adds it to the
game's index (creating missing folders) and prints its download URL as the
last line:

```
Upload deck.png (12345 bytes) to Decks/Old:
  create folder  Decks
  create folder  Decks/Old
  write          <SHA1>_deck.png
  share          <SHA1>_deck.png
  add entry      Decks/Old/deck.png
Upload? [y/N] y
Backed up the cloud index to 20261112T101500Z.
https://steamusercontent-a.akamaihd.net/ugc/<handle>/<SHA1>/
```

- Without `--yes` ttskit asks before changing anything; when it cannot ask
  (stdin is not a terminal) it stops and asks for `--yes`. `--dry-run` only
  prints the plan.
- `--name NAME` stores the file under another name than its file name.
- The cloud file name is `<SHA1>_<name>`, so the same name with the same
  content is uploaded only once: uploading it again to the same folder prints
  the existing URL and changes nothing. The cloud name does not include the
  folder, so the same name and content in a second folder would be the same
  cloud file; ttskit refuses that and suggests `--name`.
- A file with the same name but different content in the target folder is
  uploaded as a second file, as the game does, with a warning. A changed
  file always gets a new URL.
- If an upload stops after the file was written (for example sharing
  failed), running the same command again finishes it without writing the
  file again.
- Index entries get the date in the game's format (`MM/dd/yyyy HH:mm:ss`,
  local time).

`cloud url NAME` prints the URL of a file in the index. `NAME` is a folder
path (`Decks/Old/deck.png`, either separator), a display name, or the cloud
name `<SHA1>_<name>`, matched case-insensitively; if several files match,
ttskit lists them. A file that has no URL yet is shared with `--share`
(same `--dry-run` / `--yes` rules as upload). `cloud url` never changes
anything without `--share`.

Both commands accept `--json` and print one JSON object.

Limits: a single file can be at most 100 MiB (the limit of Steam's
`FileWrite`). Before writing, ttskit checks the free cloud space that Steam
reports and refuses uploads that do not fit, so nothing is changed. If Steam
refuses a write and the space is short, the error says so.

### Delete

```
ttskit cloud delete Decks/Old/deck.png          # show what would be deleted
ttskit cloud delete Decks/Old/deck.png --yes    # delete it
ttskit cloud delete --folder Decks/Old --yes    # delete a folder and everything below it
```

`cloud delete NAME` deletes one file from the cloud and removes it from the
game's index. `NAME` works as in `cloud url`; a file that is in the cloud but
not in the index (`cloud list` shows it under `Not in index:`) is given by
its cloud name. `--folder F` deletes every file in `F` and its subfolders and
removes `F` and the subfolders from the index; the root folder cannot be
deleted. Deleting the last file of a folder keeps the folder. The game's own
files (the index files, `WorkshopUpload`, `WorkshopImageUpload.png`) are
never deleted.

```
Delete folder Decks/Old (2 files, 2 folders):
  delete file    Decks/Old/back.png  (<SHA1>_back.png)
  remove entry   Decks/Old/Sub/gone.png  (<SHA1>_gone.png, file already missing)
  remove folder  Decks/Old/Sub
  remove folder  Decks/Old
warning: 1 file has a download URL; saves that use it will no longer load it
Dry run: nothing was changed. Run again with --yes to delete.
```

- Without `--yes` nothing is changed; there is no question to answer.
- With `--yes` ttskit first backs up the index (also when only a file that
  is not in the index is deleted), then deletes the cloud files, then
  updates the index. If it stops part way, running the same command again
  finishes it.
- Deleted files are gone for good, and their download URLs stop working.
- `--json` prints one JSON object with the planned files and folders, and
  after `--yes` the deleted cloud files and the backup name.

### Status

```
ttskit cloud status ./assets --folder Decks
ttskit cloud status ./assets --json
```

`cloud status DIR` compares a local directory with a cloud folder (`--folder`,
default the root) and changes nothing. Subdirectories of `DIR` correspond to
subfolders; files and directories whose name starts with `.` and symbolic
links are skipped. A local file and a cloud file correspond when they have
the same folder and name (ignoring case); their content is compared by SHA1,
which is part of the cloud name, so nothing is downloaded.

```
Compare ./assets with cloud folder Decks:
  changed    back.png
  new        front.png
  missing    old.png
  broken     sub/b.png  (cloud file is gone)
1 new, 1 changed, 1 missing, 1 broken, 3 identical
```

- `new`: only local. `changed`: both, with different content. `missing`:
  only in the cloud folder. `identical`: same content (not listed in the
  text output, only counted). `broken`: same content, but the index entry's
  cloud file is gone, so the URL no longer works.
- The folder may not exist in the cloud yet; then every file is `new`.
- `--json` lists every file with its state, local size and SHA1, cloud name
  and URL, plus the counts.
- Exit code: 0 when everything is identical, 2 when anything differs, 1 on
  errors.

### Project config and sync plan

A project can name its cloud folder and local directory in a `ttskit.toml`:

```toml
[cloud]
folder = "MyGame"   # one cloud folder per game

[local]
dir = "assets"      # relative to the directory holding ttskit.toml
```

`assets/Boards/a.png` then belongs to the cloud folder `MyGame/Boards`. Both
keys are required, unknown keys are refused, the cloud folder cannot be the
root or contain `.`/`..` levels, and the local directory must be relative and
free of `..`.

The `ttskit-cloud` library can turn a local listing and the cloud index into
a sync plan (`plan_sync`). It reads and changes nothing, and prints as text or
JSON. Each direction runs one way: `push` makes the cloud folder match the
local directory, `pull` the reverse. Per file the plan says `upload`,
`download`, `delete`, `rename`, `skip` or `conflict`, with a reason.

- Names compare ignoring case, as Steam Cloud does: same content with a
  different spelling is a `skip` (case only).
- Nothing is deleted unless prune is requested; without it, files only on the
  target side are skipped. With prune, one deleted and one new file with the
  same content form a `rename` (the cloud has no rename, so a push uploads
  and then deletes).
- Conflicts block the plan: local files that differ only by case, several
  index entries for one name, and the same name and content in two folders
  (they would be one cloud file).

### Sync: push

`ttskit cloud sync push` makes the cloud folder named in `ttskit.toml` match
the local directory (see above). It reads `./ttskit.toml`, or the file given
with `--config PATH`; the local directory is relative to that file.

```
ttskit cloud sync push --dry-run   # print the plan, change nothing
ttskit cloud sync push             # print the plan, ask, then push
ttskit cloud sync push --yes       # push without asking
ttskit cloud sync push --prune     # also delete what the local directory lacks
```

- The plan is printed first and is exactly what runs: the cloud is looked at
  again just before the push, and the push is refused if it no longer matches
  the plan. A plan with conflicts is printed and then refused, also with
  `--dry-run`.
- New files are uploaded and shared. Without `--prune` nothing in the cloud
  is deleted, and a changed file is **not** pushed (it is listed as
  `skip ... needs --prune`), because replacing it deletes the old version
  and saves that use the old URL stop loading it. With `--prune` a changed
  file is uploaded and its old version removed, files that exist only in the
  cloud folder are deleted, and a renamed file is uploaded and then
  deleted under its old name. Empty folders are left in place. A cloud file
  that another index entry still uses is not deleted.
- A real run takes one backup of the index first (see below) and writes the
  index after every file, so an interruption loses at most one step. New
  folders are added before the files that go into them.
- If a step fails, the push stops, names the backup and says so. Run the same
  command again: files that already made it into the cloud are not written
  again, and the run continues where it stopped.
- `--json` prints the plan and the result as one JSON object. Exit code 1 on
  errors.
- Not yet checked against the real Steam Cloud: a push with the game open
  (like `cloud upload`, the Cloud Manager needs a folder reload) and an
  interrupted run. Close the game for now.

Pull and prune of the local side come with the next sync issue.

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
with the game running at its main menu, and the game kept running.

`cloud upload` and `cloud delete` also work with the game open: the change
shows in the open Cloud Manager once it reloads the folder (switch to
another folder and back). For `cloud url --share` and `cloud restore --yes`
this has not been checked yet, so close the game first.

### Open questions

- Does sharing a file that is already shared return the same handle (and so
  the same URL)? Not checked against Steam yet.

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
