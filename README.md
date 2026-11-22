# ttskit

An unofficial command line toolkit (and MCP server) for developing
[Tabletop Simulator](https://store.steampowered.com/app/286160/Tabletop_Simulator/)
mods. Not affiliated with or endorsed by Berserk Games.

> **Status:** early development. The `cloud`, `save`, `editor`, `watch` and
> `mcp` commands work; see the issue tracker for the roadmap.

## Planned features

- **Save pack/unpack:** split a save into readable, diff-friendly files and
  assemble it again, byte-exact, with `require` / `<Include/>` bundling.
- **Steam Cloud management:** one cloud folder per game, sync with a local
  directory, a committed manifest of asset URLs (done), and a scan of the
  asset URLs a save uses (done).
- **External Editor API client (done):** talk to a running game to read and write
  scripts, run Lua and capture console output; `watch` pushes edits on save.
- **MCP server (done):** expose the editor API to AI assistants.

## Building

```
cargo build
cargo test
```

Windows is the primary platform at first.

See [CONTRIBUTING](CONTRIBUTING.md) for the checks to run, the test fixture
policy and how to test against real saves locally.

## Editor commands

`ttskit editor` talks to a running game through its External Editor API: `get-scripts`, `save-and-play`, `exec`, `send` and `console`. The game must be running; nothing here needs Steam. For example, watch the game's console while you work:

```
ttskit editor console --follow
ttskit editor console --json            # one-shot, one JSON object per line
```

For a fast edit loop, `ttskit watch DIR [--include LIB...]` watches an unpacked directory and, after a debounced change, bundles the scripts and UI and sends them to the running game with Save & Play (which reloads it, so unsaved changes in the game are lost). The game's `print` output and Lua errors show in the same terminal. `--once` pushes a single time for editor tasks, `--pack-to SAVE.json` also writes the packed save on every change.

```
ttskit save unpack mysave.json -o mymod
ttskit watch mymod --include lib
```

All subcommands share `--game-port`, `--editor-port`, `--timeout` and `--json`, and exit with 0 (success), 1 (failure), 2 (usage error) or 3 (the game is not reachable). Messages the game sends while no `ttskit` listener is running are lost. Details, observed behaviour and limits: [docs/editor-api.md](docs/editor-api.md).

Shell completions are not generated yet (tracked in the issue tracker); until then `ttskit editor --help` and `ttskit editor <command> --help` list every flag.

## MCP server

`ttskit mcp` runs a [Model Context Protocol](https://modelcontextprotocol.io/) server on stdio, so an AI assistant can work with the running game: read scripts, run Lua, push an edited tree with Save & Play and read the errors. It is a client of the same External Editor API as the `editor` commands, with the same safeguards; nothing here needs Steam.

| Tool | What it does |
| --- | --- |
| `get_status` | Whether the game answers, the loaded save, ports and root |
| `get_scripts` | Summary of all objects with a script or UI; with `guids`, their full text |
| `execute_lua` | Run Lua in Global or an object and return prints and Lua errors |
| `send_custom_message` | Send a JSON object to the game's `onExternalMessage` |
| `get_console` | Read buffered `print` output, Lua errors, custom messages, saves and loads |
| `save_and_play` | Send the scripts and UI of an unpacked tree and reload the game |

Safeguards and limits:

- `save_and_play` never sends on the first call: it returns the diff and a fingerprint of the plan. Only a second call with `confirm` set to that fingerprint sends, and only if the plan is still the same. Deleting a script or UI that the tree lacks also needs `allow_delete`.
- `save_and_play` only reads paths inside the server's root: the working directory, or `--root DIR`. Paths with `..` are refused.
- The server holds the editor port (39998) for its whole life and buffers what the game sends, also between tool calls (1000 events and 4 MiB by default; `--console-buffer` changes the count). Every event has a sequence number, so `get_console` with `since` returns only what is new. While the server runs, `ttskit watch`, `ttskit editor console` and other editor integrations cannot receive from the game; the server does not start if one of them already listens. See [docs/editor-api.md](docs/editor-api.md#ttskit-mcp) for integrations that listen on the wildcard address.
- `--game-port`, `--editor-port` and `--timeout` work as for the `editor` commands.

Claude Code, from the directory with the unpacked mod (writes `.mcp.json` for the project):

```
claude mcp add --scope project ttskit -- ttskit mcp
```

Any client that takes an `mcpServers` JSON configuration, such as Claude Desktop (`claude_desktop_config.json`) or a project `.mcp.json`:

```json
{
  "mcpServers": {
    "ttskit": {
      "command": "ttskit",
      "args": ["mcp", "--root", "C:\\path\\to\\mymod"]
    }
  }
}
```

Use the full path to `ttskit.exe` as `command` if it is not on `PATH`. Details: [docs/editor-api.md](docs/editor-api.md#ttskit-mcp).

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
- `steam_api64.dll` is next to `ttskit.exe`. A build with the `steam` feature
  is linked against it, so without the DLL Windows refuses to start the
  program at all, even for `ttskit --version`. `cargo run` and `cargo test`
  find it on their own. For an installed or copied executable, take the DLL
  from the build output (`target/release/build/steamworks-sys-*/out/`) or
  from the Steamworks SDK (`redistributable_bin/win64`) and put it next to
  the executable. With `cargo install --path crates/ttskit --features steam`
  that is `%USERPROFILE%\.cargo\bin`.

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
- Inside a project (`./ttskit.toml` or `--config PATH`) a finished upload is
  also recorded in the project manifest and `Updated manifest ...` is printed
  just before the URL; see "Manifest and TSV".

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
links are skipped, as are files matching the `[local] ignore` patterns of
`./ttskit.toml` or `--config PATH` (see "Project config and sync plan"). A
local file and a cloud file correspond when they have
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

An optional `ignore` list keeps files out of sync:

```toml
[local]
dir = "assets"
ignore = ["*.log", "*.json", "**/*_debug_*", "tmp/"]
```

- Patterns use gitignore syntax (`*`, `**`, a leading `/` to anchor, a
  trailing `/` for directories, `!` to re-include) and are matched against
  the path relative to `dir`, ignoring case. Comments start with `#`.
- Ignored local files are not read and are skipped by `cloud sync push`,
  `cloud sync pull` and `cloud status`; the plan ends with
  `ignored: N local, M cloud`.
- Cloud files whose path (relative to the cloud folder) matches are left
  alone: never deleted or replaced by `--prune`, not listed as only in the
  cloud, and not downloaded by `pull`.
- `cloud status` reads the patterns from `./ttskit.toml` if it exists, or
  from `--config PATH`; they apply relative to the `DIR` you pass.
- A pattern the matcher refuses makes the config invalid.

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
- The summary shows how many bytes are uploaded, e.g. `94 upload (118.3 MiB)`,
  and a line `cloud: 113.3 MiB used, 93.0 GiB free` shows the Steam Cloud quota
  (left out if Steam cannot report it; `cloud list` and `cloud status` print
  the same line). A push whose new files do not fit into the free space is
  refused before anything is written, also with `--dry-run`. Sizes use binary units (1 GiB = 1024³ bytes), like the in-game cloud manager.
- `--json` prints the plan and the result as one JSON object (with `quota`:
  `total`, `used`, `available` in bytes, or `null`, and `plan.transfer_bytes`). Exit code 1 on
  errors.
- A push that completes (also when there was nothing to do) refreshes the
  project manifest, see "Manifest and TSV" below. Dry runs and failed pushes
  leave it alone.
- Not yet checked against the real Steam Cloud: a push with the game open
  (like `cloud upload`, the Cloud Manager needs a folder reload) and an
  interrupted run. Close the game for now.

### Sync: pull

`ttskit cloud sync pull` makes the local directory match the cloud folder named
in `ttskit.toml`, the reverse of push. It reads the same config and options.

```
ttskit cloud sync pull --dry-run   # print the plan, change nothing
ttskit cloud sync pull             # print the plan, ask, then pull
ttskit cloud sync pull --yes       # pull without asking
ttskit cloud sync pull --prune     # also remove local files the cloud lacks
```

- The plan is printed first and is exactly what runs: the cloud is looked at
  again just before the pull, and the pull is refused if it no longer matches.
  A plan with conflicts is printed and then refused, also with `--dry-run`.
- The cloud is only read, never changed, so no index backup is taken.
- New cloud files are downloaded. A cloud file that differs from the local
  one **replaces** it; this is listed as `download ... (changed)` and needs the
  confirmation (or `--yes`). Before overwriting, ttskit checks that the local
  file is still the one that was scanned, and refuses otherwise.
- Without `--prune` nothing local is removed; local-only files are listed as
  kept. With `--prune` they are listed first and then moved to the recycle
  bin, not deleted for good. Empty directories are left in place.
- Every download is checked against the SHA1 in the index and written
  atomically. Names from the cloud that are not safe local paths (`..`,
  characters or names invalid on Windows, names starting with `.`) make the
  pull refuse before anything is changed.
- If a step fails, the pull stops and says so; run the same command again to
  finish. A local directory that does not exist yet is created.
- `--json` prints the plan and the result as one JSON object.
- Not yet checked against the real Steam Cloud.

### Manifest and TSV

A project keeps a manifest, `ttskit.manifest.json`, next to `ttskit.toml`. It
lists every shared file of the project, so it can be committed instead of
keeping URL lists by hand:

```json
{
  "version": 1,
  "entries": [
    {
      "path": "Boards/a.png",
      "cloud_path": "MyGame/Boards/a.png",
      "sha1": "A9993E364706816ABA3E25717850C26C9CD0D89D",
      "size": 1234,
      "url": "https://steamusercontent-a.akamaihd.net/ugc/1234567890/A9993E36.../"
    }
  ]
}
```

- `path` is relative to the local directory of `ttskit.toml` (`/` separated);
  it is left out for files that are not in the project directory.
  `cloud_path` is the cloud folder plus the display name.
- The output is deterministic: entries are sorted by cloud path and then local
  path (ASCII case-insensitive), the field order is fixed and the file ends
  with a newline, so it diffs well in git.
- `cloud sync push` refreshes the manifest for the whole local directory after
  a completed run. A changed local file that was not pushed (no `--prune`) is
  not linked to the old cloud version, and files that no longer exist locally
  are dropped. Entries of ignored files (see "Project config and sync plan")
  are left as they are.
- `cloud upload FILE` adds or replaces one entry. It looks for `./ttskit.toml`
  (or `--config PATH`); without a project it does not write a manifest. A file
  outside the local directory gets an entry without `path`.
- A manifest that cannot be updated after a successful cloud change is
  reported as an error saying the cloud work itself succeeded.

```
ttskit cloud manifest             # print the manifest (JSON)
ttskit cloud manifest --tsv       # print filename<TAB>URL, one line per entry
ttskit cloud import-tsv list.tsv  # adopt an existing list
```

- `cloud manifest` is local only and needs no Steam connection. For entries
  without a local path, the TSV uses the cloud path as the filename.
- `cloud import-tsv FILE --config PATH` reads lines of `filename<TAB>URL`
  (blank lines and lines starting with `#` are skipped; a BOM and CRLF are
  fine). Each URL must belong to a file in your own cloud: it is matched
  against the cloud index by handle and SHA1, and nothing is uploaded. The
  size and cloud path come from the index. The plan (`add`, `update`,
  `unchanged`) is printed first; `--dry-run` writes nothing, `--yes` skips the
  question, `--json` prints the result as one JSON object.
- Every malformed line is reported with its line number (no tab, more than one
  tab, an empty filename or URL, a URL that is not a cloud URL or not in the
  index, an absolute or `..` filename, a duplicate filename), and nothing is
  imported if there is any.
- Exporting with `--tsv` and importing the result into an empty manifest gives
  the same file again (for entries that have a local path).

### Scan a save for asset URLs

```
ttskit cloud scan save.json                 # list the asset URLs of a save
ttskit cloud scan save.json --json          # the same as one JSON object
ttskit cloud scan save.json --occurrences   # also list where each URL occurs
ttskit cloud scan save.json --adopt         # also adopt your own cloud files
```

- Every string that starts with `http://` or `https://` counts, wherever it
  is in the save, so fields unknown to ttskit are not missed. The key holding
  the URL is only used as a label.
- The result is grouped by host (`imgur`, `steamusercontent`, `other`) and
  sorted by URL; each URL shows its labels and, if it occurs more than once,
  how often. Plain `scan` is local only and needs no Steam connection.
- Every URL also has its `occurrences`: the JSON `path` (for example
  `ObjectStates[3].CustomDeck.1.FaceURL`) and the `nickname` and `guid` of the
  nearest enclosing object that has a `GUID` (null if there is none). `--json`
  always includes them; the text output lists them with `--occurrences`. This
  maps a dead URL back to the object it belonged to.
- `--adopt` matches the Steam Cloud URLs of the save against the index of your
  own cloud (by handle and SHA1) and adds them to the manifest without
  uploading anything. Adopted entries have no local `path`. URLs that are not
  in your cloud are listed as `unmatched` and not adopted; imgur and other
  hosts are ignored. `--config PATH` selects the project, `--dry-run` writes
  nothing, `--yes` skips the question, and `--json` adds an `adopt` object.

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

## Image tools

`ttskit image` needs a build with the `image` feature; without it the command
exits with an error that names the feature, and the image decoders are not
built at all:

```
cargo build --release --features image
ttskit image info card.png deck/
ttskit image info --json deck/
```

`image info` is read-only and tells you whether the game can use each image,
and why not. It takes files and directories (scanned recursively for png, jpg,
webp, gif, bmp and tiff, sorted by path) and prints format, size, colour
layout, bit depth and frame count, then the problems it found:

- `error`: the game rejects the file (gif, bmp, tiff, CMYK JPEG, animated
  WebP);
- `warning`: it loads, but not as the file suggests (an embedded colour
  profile the game ignores, an animated PNG shown as a still, a size of 8192
  px or more that loads slowly, no fully opaque pixel so no token can be made);
- `note`: translucent pixels, which a token outline ignores.

It exits with status 1 if a file could not be read or has errors; warnings and
notes do not change the exit status. `--json` prints one JSON object with
`images` (each with `compatible` and `problems`, whose `code` values are
stable) and `errors`.

### Checking image sizes

```
ttskit image check deck/
ttskit image check deck/ --max-side 2048 --max-bytes 2m
ttskit image check --json deck/
```

`image check PATH...` finds images that are larger than the game can use well.
It reads only the file header, except that a PNG or WebP with an alpha channel
is decoded to see whether any pixel is actually transparent. Each file gets
its size, format and alpha flag, then findings with a stable rule id:

- `side-too-long`: a side over `--max-side` (default 4096, the largest size
  seen to load fine in the game; 8192 loaded slowly);
- `file-too-large`: over `--max-bytes` (no default; `k`, `m` and `g` suffixes
  are 1024-based);
- `extension-mismatch`: the extension does not match the real format;
- `alpha-opaque`: an alpha channel in which every pixel is fully opaque;
  `image optimize` removes it losslessly.

Exit status: 0 without findings, 2 with findings, 1 if a file could not be
read. `--json` prints one object with `max_side`, `max_bytes`, `images` (each
with `findings`) and `errors`. Where the limits come from is in
[docs/image-formats.md](docs/image-formats.md).

### Resizing images

```
ttskit image resize deck/ --max-side 4096 --out-dir resized
ttskit image resize card.png --width 1024 --out card.small.png
ttskit image resize deck/ --scale 0.5 --dry-run
ttskit image resize deck/ --max-side 2048 --in-place --yes
```

`image resize INPUT...` scales PNG and JPEG images down and keeps the aspect
ratio. Give exactly one of `--max-side N` (the longer side), `--width N`,
`--height N` or `--scale F`. The default filter is Lanczos 3 (`--filter` picks
`catmull-rom`, `triangle` or `nearest`); transparency is resampled without
fringes and 16-bit PNG stays 16-bit.

- An image is never enlarged unless you pass `--allow-enlarge`. One that would
  grow, or that already has the target size, is left as it is (with `--out` or
  `--out-dir` a copy of the original goes to the output).
- PNG stays PNG and JPEG stays JPEG; a JPEG is re-encoded at `--quality`
  (default 85). Other formats are skipped with a note: run `image convert`
  first.
- The colour profile and EXIF data are dropped, as in `image convert`.
- Output rules are those of the other image commands: say where results go
  (`--out`, `--out-dir`, or `--in-place --yes`), existing files are never
  overwritten without `--yes`, `--dry-run` writes nothing, and the same input
  and options give the same bytes.

### Converting images

```
ttskit image convert photo.webp card.bmp --to png --out-dir converted
ttskit image convert art.png --to jpg --quality 90 --background "#ffffff"
ttskit image convert deck/ --to png --out-dir converted --dry-run
```

`image convert INPUT... --to png|jpg` turns images into the two formats the
game loads. Inputs are files or directories (scanned recursively); PNG, JPEG,
WebP, BMP, TIFF and GIF are read, and the format is decided by content, not by
the extension.

- Output goes to `--out FILE` (exactly one input), to `--out-dir DIR` (created
  if missing; each file keeps its name with the new extension), or, with
  neither, next to the input.
- Nothing is overwritten without `--yes`, and a run is refused before anything
  is written if two inputs would write the same file or an output is another
  input of the run. `--dry-run` converts in memory and prints what would be
  written. Files are written atomically. Each file is converted on its own: a
  failure is reported and the other files still convert; the exit status is 1
  if any failed.
- Output is deterministic: the same input and options give the same bytes.
- `--json` prints one object with `dry_run`, `conversions` (input, output,
  formats, size, `overwrote` and `notes` with stable codes) and `errors`.

JPEG output:

- `--quality N` (1 to 100, default 85).
- An image with any transparent or translucent pixel is refused unless
  `--background COLOR` (`#RRGGBB`, `#RGB`, `white` or `black`) says what to
  flatten it onto, so alpha is never lost silently. An alpha channel that is
  fully opaque needs no background.
- Channels are 8-bit; deeper input is reduced, with a note.

PNG output keeps alpha and 16-bit channels. A 32-bit float TIFF is stored as
16-bit.

Policy for the rest, from what the game accepts: an animated input (GIF,
animated WebP or PNG) converts its first frame, with a warning; a CMYK JPEG
becomes RGB; the embedded colour profile and EXIF data are dropped, since the
game ignores them, unless `--keep-metadata` is given. Before the EXIF data is
dropped, its orientation is applied to the pixels, so a rotated photo stays
upright. Converting a file to its own format re-encodes it, which also strips
metadata and normalises progressive, interlaced or CMYK files.

### Optimizing PNG files

```
ttskit image optimize deck/ --dry-run
ttskit image optimize card.png --out card.small.png
ttskit image optimize deck/ --out-dir optimized --level 4
ttskit image optimize deck/ --in-place --yes
```

`image optimize INPUT...` shrinks PNG files without changing a single pixel, so
mods load faster and use less Steam Cloud space. It uses the
[oxipng](https://crates.io/crates/oxipng) library; no other program is needed.
The decoded pixels (alpha included, also under fully transparent pixels) are
compared before and after for every file, and a file whose result differs is
refused.

- You must say where results go: `--out FILE` (exactly one input),
  `--out-dir DIR` (created if missing), or `--in-place --yes`. `--dry-run`
  optimizes in memory, reports the savings and writes nothing; on its own it
  needs no destination.
- A file is only replaced when the result is smaller. In place, a file that did
  not shrink is not touched. With `--out` or `--out-dir`, such a file is copied
  unchanged, so the output always holds every PNG. Existing outputs are never
  overwritten without `--yes`, and a run is refused before anything is written
  if two inputs would write the same file. Files are written atomically.
- The report shows old size, new size and the saving per file, plus a total.
  `--json` prints `files` (`input`, `output`, `status` of `optimized`,
  `unchanged` or `skipped`, `original_bytes`, `optimized_bytes`, `saved_bytes`,
  `written`, `note`), `totals`, the `level` and `strip` used, and `errors`.
- Files that are not PNG (decided by content, not by extension) are skipped
  with a note, not an error. A file that cannot be read as PNG is an error for
  that file only; the others still run, and the exit status is 1.
- `--level 0` to `6` (default 2): higher is smaller and slower.
- `--strip safe|all|none` (default `safe`). `safe` removes chunks that do not
  affect how the image is shown (text, EXIF, gamma and similar) and keeps the
  colour profile, pixel density and animation chunks. `all` also removes the
  colour profile; pixels stay identical, but a viewer that honours the profile
  may show other colours. An animated PNG is skipped under `all`, since that
  would remove the animation. `none` keeps every chunk.
- The interlacing is kept and the colour of transparent pixels is never
  altered.
- The output is deterministic: the same input, `--level` and `--strip` give the
  same bytes, so an optimized file is stable (running the command again changes
  nothing) and `cloud sync push` does not upload it again. Upgrading ttskit can
  change the bytes, because a newer oxipng may compress differently; the files
  then count as changed once.

### Quantizing PNG files (lossy)

```
ttskit image quantize deck/ --dry-run
ttskit image quantize card.png --out card.small.png --colors 64
ttskit image quantize deck/ --out-dir small --quality 60-90
ttskit image quantize deck/ --out-dir small --engine pngquant
ttskit image quantize deck/ --in-place --yes
```

`image quantize INPUT...` turns PNG files into indexed PNGs with a palette of at
most 256 colours (alpha included), which is often 70 to 90 percent smaller for
art and photos. **It is lossy**: pixels change, and every report starts with a
note saying so. The result is then run through the same lossless optimizer as
`image optimize`, and the smaller file wins.

- Destinations, `--dry-run`, `--yes`, atomic writes and the refusal rules are
  the same as for `image optimize`: you must give `--out FILE` (exactly one
  input), `--out-dir DIR` or `--in-place --yes`, or use `--dry-run` on its own.
  Existing outputs are never overwritten without `--yes`. A file is only
  replaced when the result is smaller; otherwise it is left alone in place, or
  copied unchanged with `--out` / `--out-dir`.
- `--colors N` (2 to 256, default 256): largest palette size. An image that
  already has at most N distinct colours keeps them exactly.
- `--quality MIN-MAX` (each 0 to 100): a file whose result scores below MIN is
  skipped and left unchanged; a skipped file gets no output, even with `--out`
  or `--out-dir`. With the built-in engine, colours are also halved as long as
  the score stays at or above MAX, so a high MAX keeps more colours.
- `--dither F` (0.0 to 1.0, default 1.0): strength of the error diffusion that
  hides banding. `0` turns it off, which is smaller but more visible on
  gradients.
- `--level 0` to `6` (default 2) and `--strip safe|all|none` (default `safe`)
  control the final lossless pass as for `image optimize`.
- The file is re-encoded, so the colour profile, pixel density, text and other
  chunks are dropped whatever `--strip` says, and 16-bit channels become 8-bit.
  Animated PNGs and files that are not PNG are skipped with a note.
- The quality score is a number from 0 to 100. With the built-in engine it is
  derived from the PSNR between the original and the result, computed on
  alpha-premultiplied pixels: 20 dB or less is 0, 45 dB or more is 100. With
  pngquant, `--quality` is passed to it and uses its own scale; the score shown
  is still the built-in one, measured against the original.

Engines:

- `builtin` (default): the median cut and k-means quantizer inside ttskit.
  Nothing else is needed and the output is deterministic: the same input and
  options give the same bytes.
- `pngquant`: runs your own [pngquant](https://pngquant.org/) executable
  (`--engine pngquant` finds it on `PATH`; `--pngquant PATH` names it and
  implies the engine). It usually gives better quality, especially on smooth
  transparency, but it has to be installed separately and its output depends on
  its version, so it can differ between machines. It is started directly, not
  through a shell. If it cannot be started the run stops with exit status 1
  before any file is written. ttskit does not link libimagequant (GPL); pngquant
  is only used if you install it yourself.

The report shows old size, new size, saving, palette size and quality per file.
`--json` prints one object with `dry_run`, `lossy` (always `true`), `engine`,
`colors`, `quality` (`"MIN-MAX"` or `null`), `dither`, `level`, `strip`, `files`
(`input`, `output`, `status` of `quantized`, `unchanged` or `skipped`, `code`
for skipped files such as `quality-too-low`, `not-png` or `animated`,
`original_bytes`, `quantized_bytes`, `saved_bytes`, `colors`, `quality`,
`written`, `note`), `totals` and `errors`. A file that cannot be read is an
error for that file only; the others still run, and the exit status is 1.
Skipped files are not errors.

Which formats, animated images, 16-bit channels and colour profiles the image
commands accept or produce follows what the game itself accepts; see
[docs/image-formats.md](docs/image-formats.md).

## License

[MIT](LICENSE)
