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
- **Asset backup (done):** `save fetch-assets` downloads the asset URLs of a save into a
  local directory with a stable `index.json`; reruns fetch only what is missing.
- **URL rewrite (done):** `save rewrite-urls` replaces asset URLs in a save from a
  mapping, losslessly.
- **Rehost (done):** `save rehost` moves assets from other hosts into the project's Steam
  Cloud folder and rewrites a copy of the save.
- **Save lint (done):** `save lint` checks a save for duplicate GUIDs, bad URLs,
  Lua and XML syntax errors, oversized script state and broken decks.
- **Save diff (done):** `save diff` shows which objects changed between two saves.
- **Dead-link check (done):** `save check-urls` tells which asset URLs of a save no
  longer load.
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

## Shell completions

`ttskit completions <shell>` prints a tab-completion script for subcommands and
options to stdout. Supported shells: `powershell`, `bash`, `zsh`, `fish` and
`elvish`.

PowerShell: add this line to your profile (`notepad $PROFILE`):

```powershell
ttskit completions powershell | Out-String | Invoke-Expression
```

bash: add this line to `~/.bashrc`, or save the output to a file in
`~/.local/share/bash-completion/completions/ttskit`:

```sh
source <(ttskit completions bash)
```

zsh and fish: save the output as `_ttskit` in a directory on your `fpath`, or
as `~/.config/fish/completions/ttskit.fish`.

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

Sources can name pushed assets by `asset:{PATH}` placeholders (PATH relative to the project's local directory) instead of URLs. `ttskit save pack` resolves them from `ttskit.manifest.json` (config: `--config`, else `ttskit.toml` in the current directory) and refuses stale entries unless `--allow-stale`; `ttskit save unpack --assets` writes placeholders for URLs that exactly match a manifest entry. See [docs/pack-layout.md](docs/pack-layout.md#asset-placeholders).

All subcommands share `--game-port`, `--editor-port`, `--timeout` and `--json`, and exit with 0 (success), 1 (failure), 2 (usage error) or 3 (the game is not reachable). Messages the game sends while no `ttskit` listener is running are lost. Details, observed behaviour and limits: [docs/editor-api.md](docs/editor-api.md).

Shell completions are not generated yet (tracked in the issue tracker); until then `ttskit editor --help` and `ttskit editor <command> --help` list every flag.

## Checking asset URLs

`ttskit save check-urls SAVE.json` finds the asset URLs of a save that no longer load and the objects that use them. It needs the network but neither Steam nor the game.

```
ttskit save check-urls save.json                  # list dead and unreachable URLs
ttskit save check-urls save.json --json           # every URL with its result
ttskit save check-urls save.json --verify-content # also reject HTML error pages
ttskit save check-urls save.json --skip-host imgur.com
```

- Each distinct URL is requested once with `HEAD`; a host that refuses `HEAD` (400, 403, 405, 501) is asked with a ranged `GET` instead. Redirects are followed (up to 5) and the final answer decides.
- Results: `ok`; `redirect` (the target loads; not a problem); `dead` (HTTP error status); `unreachable` (network error, timeout, redirect loop); `skipped` (excluded with `--host HOST`, which checks only that host and its subdomains, or `--skip-host HOST`).
- Timeouts, network errors, `429` and `5xx` are retried (`--retries`, default 3, with a growing pause). `--timeout` (default 10 s per request), `--concurrency` (default 16) and `--per-host` (default 4) tune the load; requests identify as `ttskit/<version>`.
- `--verify-content` fetches the first 1 KiB of each file and counts a `2xx` answer as `dead` when it is an HTML page (by `Content-Type` or by its first bytes), since some hosts answer 200 with a placeholder page. It does not check that the file is a valid image or model.
- The text output lists each dead or unreachable URL with the JSON path, nickname and GUID of every use, then redirects and a summary line. `--json` lists all URLs sorted, with `result`, `status`/`target`/`reason`, `labels` and `occurrences`.
- Exit status: 0 when nothing is dead or unreachable, 2 when something is, 1 on errors.

## Save lint

`ttskit save lint SAVE.json` checks a save for common mistakes: GUIDs, positions, asset URL fields, Lua and XML syntax, script state and deck data. It works on the file alone: no network, no Steam, no game. (Whether a URL still loads is a different question, answered by [`save check-urls`](#checking-asset-urls).)

```
ttskit save lint save.json                     # findings at warning level or above
ttskit save lint save.json --level info        # also show info findings
ttskit save lint save.json --json              # machine-readable
ttskit save lint save.json --allow insecure-url --deny missing-guid
ttskit save lint --list-rules                  # every rule with its default severity
```

- Findings are grouped by the object they belong to (`Nickname (GUID)`; findings outside any object under `(save)`), each line giving severity, rule, JSON path and a message. A summary line counts the findings and says how many were hidden because they are below `--level`.
- `--level info|warning|error` (default `warning`) sets the lowest severity that is shown and counted. `--json` prints `{save, level, summary: {error, warning, info}, findings: [{rule, severity, message, path, nickname, guid}]}`.
- `--allow RULE` switches a rule off; `--warn RULE` and `--deny RULE` make it a warning or an error. All three repeat, apply after the config file, and an unknown rule id is an error.
- Exit status: 0 when nothing at or above the level is found, 2 when something is, 1 on errors (unreadable save, invalid JSON, bad config, unknown rule id or level).

### Configuration

Settings live in the `[lint]` table of `ttskit.toml`, which `save lint` reads from the current directory if the file exists, or from `--config FILE` (which must exist). Other tables in the file (`[cloud]`, `[local]`, `[image]`) are ignored by lint, and `[lint]` is ignored by the other commands, so one file serves all of them.

```toml
[lint]
state_limit = 1048576        # bytes of LuaScriptState before large-script-state fires (default 1 MiB)

[lint.rules]
insecure-url = "off"         # off, info, warning or error
missing-guid-nested = "warning"
```

### Rules

| Rule | Default | What it finds |
|---|---|---|
| `deck-id-out-of-range` | error | a DeckIDs entry points outside the object's CustomDeck sheets, or a card id is negative or not an integer |
| `deck-size` | warning | a CustomDeck sheet grid is outside the editor limits (1-10 by 1-7) |
| `duplicate-guid` | error | the same GUID is used by more than one object |
| `duplicate-guid-nested` | info | a GUID repeats and at least one of the objects is inside a container or a state |
| `empty-url` | error | a required URL field is empty |
| `insecure-url` | warning | a URL field uses http instead of https |
| `invalid-url` | error | a URL field is not a valid http(s) URL |
| `large-script-state` | warning | a LuaScriptState is longer than the configured state_limit |
| `legacy-steam-url` | info | a URL uses the legacy http://cloud-3.steamusercontent.com host; `ttskit save rewrite-urls --normalize-steam-hosts` fixes it |
| `local-url` | error | a URL field points at a file on the local machine |
| `lua-syntax` | warning | a LuaScript has a syntax error by Lua 5.2 grammar (the game also accepts `!=` and `\|x\| expr` lambdas, which are reported) |
| `missing-guid` | warning | an object has no GUID, or it is not a non-empty string |
| `missing-guid-nested` | info | an object inside a container or a state has no GUID, or it is not a non-empty string |
| `non-finite-number` | error | a Transform field is NaN or infinite |
| `position-out-of-bounds` | warning | an object's position is far outside the playable area |
| `script-state-not-json` | warning | a LuaScriptState is not JSON although its script calls JSON.decode |
| `xml-malformed` | error | an XmlUI is not well-formed XML |

Notes:

- **Local only.** The URL rules look at the text of URL fields (keys ending in `URL`, such as `FaceURL`, `MeshURL`, the `URL` of `CustomUIAssets` items). They do not contact any host. A value like `<Name>file` is a game-internal placeholder and is skipped; a value starting with `{de}` style language markers holds one URL per language and each is checked; a leading `{verifycache}` is ignored.
- **Lua syntax.** The Lua check parses the Lua 5.2 grammar only. The game's Lua dialect accepts a few more forms, notably `!=` and the `|x| expr` lambda shorthand; these are reported (by default as warnings), so expect findings on scripts that work in the game. Scripts are only parsed, never run. `#include` lines of the editor are ignored.
- **GUID rules are split.** The game repeats GUIDs on copies of an object inside different containers, so `duplicate-guid` and `missing-guid` look only at the top-level objects of the save, and `duplicate-guid-nested` and `missing-guid-nested` (info by default) cover objects inside containers and states.
- **Card ids.** `deck-id-out-of-range` checks `DeckIDs` as `sheet * 100 + card` against the object's own sheets. A card's own `CardID` is only checked to be a non-negative integer, because cards taken out of a deck keep the numbering of that deck.
- **Speed.** Linting runs in a single pass over the save. With a release build, a 20 MB save with hundreds of thousands of values lints in about 2 seconds in total, most of it parsing Lua.

## Save diff

`ttskit save diff OLD.json NEW.json` shows what changed between two saves in terms of objects, not JSON lines. It works on the files alone: no network, no Steam, no game.

```
ttskit save diff old.json new.json                    # short summary
ttskit save diff old.json new.json --verbose          # every change with old and new values
ttskit save diff old.json new.json --scripts          # also the line diff of changed scripts and XML UI
ttskit save diff old.json new.json --tolerance 0.001  # ignore float noise in transforms
ttskit save diff old.json new.json --json             # machine-readable
```

- Objects are matched by GUID, also inside containers (bags, decks) and states, so an object that moved to another container is reported as moved, not as removed and added. Each object is reported as added, removed, moved or changed, with the kinds of change: `transform`, `script`, `xml_ui`, `script_state`, `assets` (URL fields, old and new) and `other` (listed by JSON path).
- The global parts are compared too: global script, UI and script state, table, sky, lighting, hands, tab states and custom UI assets.
- Numbers are compared as text, so `1.0` and `1` differ. `--tolerance EPSILON` makes transform numbers within EPSILON count as equal; it does not apply to other fields.
- An object without a GUID, or with a GUID that occurs more than once in either save, is ambiguous. It is paired with the object at the same index in the same parent list; the rest is reported as added or removed. Such objects are flagged, and repeated GUIDs are listed.
- Output is sorted and stable. Exit status: 0 when the saves are equal, 2 when they differ, 1 on errors (unreadable file, invalid JSON, bad tolerance). Repeated GUIDs alone do not make saves different.
- To review script changes line by line, unpack both saves and use a file diff; this command answers "which objects changed".

## Asset backup

`ttskit save fetch-assets SAVE.json --out DIR` downloads every asset URL of a save (images, models, sounds, ...) into a local directory, so a mod survives the day a host deletes its files. It needs the network but neither Steam nor the game, and it never modifies the save.

```
ttskit save fetch-assets save.json --out backup            # download what is missing
ttskit save fetch-assets save.json --out backup --dry-run  # print the plan, create nothing
ttskit save fetch-assets save.json --out backup --verify   # also rehash the files already there
ttskit save fetch-assets save.json --out backup --max-size 64m --skip-host example.com
ttskit save fetch-assets save.json --out backup --json     # machine-readable result
ttskit save fetch-assets save.json --out backup --cache-dir MODS   # copy from the game's cache first
ttskit save fetch-assets save.json --out backup --cache-dir MODS --offline   # cache only, no network
```

Layout of the output directory:

```
backup/index.json                  which URL is stored in which file
backup/files/<sha256>.<type>       the downloaded files
backup/.partial/                   downloads in progress; removed at the end
```

File names come only from the SHA-256 of the content and a fixed list of extensions, so no URL text reaches the file system, and identical content from several URLs is stored once.

`index.json` is pretty printed with LF line endings and has no timestamps, so it is stable and can be committed:

```json
{
  "version": 1,
  "files": [
    {
      "url": "https://example.com/a.png",
      "file": "files/3b4c...e1.png",
      "size": 18342,
      "sha256": "3b4c...e1",
      "type": "png"
    }
  ]
}
```

- `version` is `1`; a different version is an error. `files` is sorted by `url` (byte order) and has one entry per URL: `url` as written in the save, `file` the path relative to the directory (forward slashes), `size` in bytes, `sha256` lowercase hex of the content, `type` the file extension.
- Entries of other saves are kept: use one directory for several saves, each run adds its URLs. Entries are never removed.
- The type is detected in this order: the first bytes of the file (magic numbers of PNG, JPEG, GIF, WebP, WAV, PDF, Ogg, MP3, MP4 and Unity asset bundles), then the `Content-Type` header, then the extension of the URL path, else `bin`. Only a fixed list of types is used.

Behavior:

- The plan is printed first: `N URLs: P already present, D to download, S skipped`, plus `K invalid (will be downloaded again)` with the reasons, and with `--verify` the number of `verified` files. `--dry-run` stops there and creates nothing, not even the directory.
- Resume: a URL that is in the index and whose file exists with the recorded size counts as present and is not requested again, so an interrupted run or a rerun downloads only what is missing. `--verify` also rehashes those files; one that does not match its recorded SHA-256 is invalid and downloaded again. An index entry whose file is missing or has the wrong size is invalid as well.
- Files are written as `.partial` files and moved into `files/` only when complete, and `index.json` is replaced atomically, so an aborted run leaves no half-written file under its final name. An `index.json` that cannot be parsed is an error (exit 1) and is never overwritten.
- Only `http` and `https` URLs are downloaded. Redirects are followed (up to 5). An answer that is an HTML page (by `Content-Type` or by its first bytes) is rejected as a failure, since some hosts answer 200 with an error page.
- Timeouts, network errors, `429` and `5xx` are retried (`--retries`, default 3, with a growing pause).
- `--host HOST` (only that host and its subdomains) and `--skip-host HOST` are repeatable and work as in `check-urls`.
- Limits and defaults: `--max-size` 512m (bytes with an optional `k`, `m` or `g` suffix, binary multiples; a larger file fails, and download stops as soon as the limit is exceeded), `--concurrency` 16, `--per-host` 4, `--timeout` 30 (seconds to connect; a whole download may take up to an hour, so large files that make progress are not cut off). Requests identify as `ttskit/<version>`.
- Output: the plan, one `failed: <url>: <reason>` line per failed URL with the JSON path, nickname and GUID of every use, then `downloaded X, failed Y` with the locations of the files and the index. There is no per-file progress. With `--json` the output is one object: `save`, `out`, `plan` (`total`, `present`, `to_download`, `skipped`, `invalid`, `verified`), `downloaded` (`url`, `file`, `size`, `sha256`, `type`), `failed` (`url`, `reason`, `occurrences`), `index_error` (`null` or a message), `offline` and `cache_dir` (as given, `/`-separated, `null` if none); each `downloaded` entry has `source` (`network` or `cache`) and with `--cache-dir` the `plan` has `in_cache`; with `--dry-run` only `plan` is filled in.
- Game cache: `--cache-dir MODS` points at the game's `Mods` folder (the one with the `Images`, `Models`, `Assetbundles`, `Audio`, `PDF` and `Text` subfolders). The path is always given explicitly; ttskit never searches for the game directory. It is useful when a host has deleted a file that the game still has cached. For every URL that must be fetched (after `--host` / `--skip-host`) the cache is looked at first and the network is used only on a miss, so a URL that is still live is also taken from the cache. `--offline` (needs `--cache-dir`) never uses the network: a URL that is not in the cache fails with `not in the cache (offline)`.
- Cache file names: a cached file is named after its URL with every character that is not an ASCII letter or digit removed, case kept and the scheme included (`http` and `https` differ), followed by an extension. For example `http://i.imgur.com/0FCZSpL.jpg` is `httpiimgurcom0FCZSpLjpg.jpg`, and `http://cloud-3.steamusercontent.com/ugc/848217372548692243/051DB8C7.../` is `httpcloud3steamusercontentcomugc848217372548692243051DB8C7...`. The extension varies (`png`, `jpg`, `obj`, `unity3d`, `mp3`/`MP3`, `ogv`, `wav`, `pdf`, ...) and is ignored: a file matches when its name without the part after the last `.` equals that key. Only the subfolders listed above are searched, in that order (`Images Raw`, `Models Raw`, `Workshop`, `Translations` and others are ignored, and there is no recursion); if several files share a key, the first folder wins and, within a folder, the smallest file name. The command fails (exit 1) if the path is not a directory or has none of those subfolders. The rule was checked by hand against a real game cache (all of about 4,000 cached URLs matched, none was ambiguous); that check is manual and not part of the test suite. The longest cached name seen had 139 characters; whether the game shortens longer names is not verified, so no truncation is applied and such a URL may be reported as not in the cache.
- Cache files are copied, never modified or moved, through the same hashing writer as downloads, then stored under `files/<sha256>.<type>`. `--max-size` applies (`larger than the size limit`), an empty file fails (`cache file is empty`) and so does a file that is an HTML page (`cached file is an HTML page`), since the game may have cached an error page. The type comes from the first bytes, then the URL, then the extension of the cache file (same fixed list), else `bin`. A cache file that cannot be read falls through to the network, or fails with `--offline`. The index is identical whichever source a file came from: `index.json` has no `source` field and stays stable.
- With `--cache-dir` the plan has the extra line `N of them found in the cache` (how many of the URLs to download the cache has; `--dry-run` prints it too and only reads file names), with `--offline` also `M not in the cache (will fail)`, and the result line reads `downloaded X (C from the cache), failed Y (...)`. Without `--cache-dir` the output is as before.
- Exit status: 0 when everything is downloaded or present, 2 when a URL failed (the others are still downloaded and indexed) or the index could not be written, 1 on errors (unreadable save, invalid `index.json`, unusable directory) and for invalid arguments such as a bad `--max-size`.

## Rewriting asset URLs

```
ttskit save rewrite-urls save.json --map urls.tsv --out new.json
ttskit save rewrite-urls save.json --map urls.tsv --dry-run
ttskit save rewrite-urls save.json --from-manifest ttskit.manifest.json --out new.json
ttskit save rewrite-urls save.json --normalize-steam-hosts --in-place --yes
```

Replaces asset URLs in a save, for example after the assets moved to another
host. It is local only. Only the matched strings change; key order, number
text and everything else stay as they were, so a diff of input and output
shows only the replaced URLs (for a save written by the game; other layouts
are rewritten in the game's layout and the plan says so).

- `--map FILE` is a TSV with `old URL<TAB>new URL` per line. Blank lines and
  lines starting with `#` are skipped; a BOM and CRLF are fine. Both columns
  must start with `http://` or `https://`. Every bad line is reported with its
  line number, and nothing is changed if there is one.
- `--from-manifest FILE` takes new URLs from a project manifest: a Steam
  Cloud URL of the save whose SHA1 is in the manifest is replaced by the
  manifest's URL for that SHA1 (the SHA1 is the content hash, so a file
  re-uploaded under a new handle keeps it). A URL that already is one of the
  manifest's URLs is left alone; a SHA1 with several different URLs in the
  manifest is an error that asks you to use `--map` for it. `--map` and
  `--from-manifest` can be combined. The index of `save fetch-assets` maps a
  URL to a local file, not to another URL, so it is not a source.
- `--normalize-steam-hosts` rewrites URLs that start with
  `http://cloud-3.steamusercontent.com/ugc/` to
  `https://steamusercontent-a.akamaihd.net/ugc/`; the rest of the URL is kept.
  It is off by default and only applies to URLs no mapping line matched. The
  two prefixes are the only ones ttskit knows; the list is short on purpose.
- Matching is exact and whole-string: `https://x/a.png` does not match
  `https://x/a.png?v=2`. Mapping lines never chain, so `A -> B` together with
  `B -> A` swaps the two. The same old URL on two lines with the same new URL
  is merged (and noted); with different new URLs it is an error.
- Scripts: URLs inside `LuaScript`, `LuaScriptState` and `XmlUI` strings are
  part of longer text. They are counted and listed separately in the plan, and
  replaced only with `--in-scripts`. A URL there is the run of URL characters
  after `http://` or `https://`, without trailing `.,;:!?)'*`; in `XmlUI`
  `&amp;` is read as `&`. URLs in other free text (descriptions, notes) are
  not touched.
- The plan is printed first: per mapping line the number of occurrences in
  values and in scripts, and the objects (nickname and GUID) they are in, then
  the lines that match nothing. `--dry-run` stops there. `--json` prints the
  plan and the result as one object, with every path, GUID and nickname.
- Output: `--out FILE` writes a new file (an existing one needs `--yes`).
  `--in-place --yes` replaces the input, through a temporary file; a save with
  nothing to replace is not rewritten. Without either (and without
  `--dry-run`) the command refuses.
- Exit status: 0 on success, also when mapping lines match nothing; 1 on
  errors (unreadable files, bad mapping, conflicts, missing `--out`).
- Rewriting A to B and then B to A gives back the original bytes.

## Rehosting assets to Steam Cloud

```
ttskit save rehost save.json --dry-run                 # print the plan, change nothing
ttskit save rehost save.json                           # ask, then move and write save.rehosted.json
ttskit save rehost save.json --host imgur --yes        # only imgur, no question
ttskit save rehost save.json --folder Imported --out new.json
ttskit save rehost save.json --in-place --yes
```

Moves the assets of a save that live on other hosts (for example imgur) into
the project's Steam Cloud folder and points a copy of the save at the new URLs,
in one command. Run it inside a project (`ttskit.toml`, see `cloud sync`); it
needs the network, a ttskit built with the `steam` feature and a running Steam.
Only rehost assets you may redistribute: other people's images and models are
not yours to publish.

The steps reuse the other commands:

1. **Select.** Every http(s) URL of the save except Steam Cloud URLs that point
   into your own cloud. `--host HOST` (only that host and its subdomains) and
   `--skip-host HOST` narrow it and are repeatable; `imgur` is short for
   `imgur.com`. URLs of other people's Steam Cloud files are selected like any
   other host.
2. **Plan.** Each URL with its size and target, the total download size and the
   free cloud quota. Sizes come from `HEAD` requests (`Content-Length`), so a
   host that does not answer shows `size unknown`. The target is
   `<cloud folder>/<folder>/files/<sha256>.<type>`; the name is only known after
   the download, so a URL that is not downloaded yet shows the pattern. The plan
   is printed before anything is downloaded. `--dry-run` stops here and
   downloads nothing. Otherwise you are asked once; `--yes` skips the question.
3. **Download** (as `save fetch-assets`) into `<local dir>/<folder>/files`, with
   `index.json` next to it. Add `<folder>/index.json` to `[local] ignore` in
   `ttskit.toml`, or `cloud sync push` will upload the download index too.
4. **Upload and share** the files (as `cloud sync push`, but only these files, never
   deleting anything) into `<cloud folder>/<folder>/files`. Identical content has
   one name, so it is uploaded once and all its URLs get the same new URL. The
   quota is checked before the first upload.
5. **Manifest.** The files are added to `ttskit.manifest.json`, with the local
   path `<folder>/files/<name>`.
6. **Rewrite** (as `save rewrite-urls`): the result goes to `SAVE.rehosted.json`
   next to the input (an existing file needs `--yes`), to `--out FILE`, or over the
   input with `--in-place --yes`. Everything but the replaced URLs stays as it
   was. URLs inside scripts are replaced only with `--in-scripts`.

Interrupting is safe. The save is written only after every upload succeeded. Run
the same command again: files already downloaded are not fetched again, files
already in the cloud are not written again, and the original save is never
modified unless you asked for `--in-place`.

- `--folder SUB` (default `rehosted`) is the sub folder of the project's cloud
  folder and local directory; it must not contain `.` or `..` levels.
- Limits and retries as in `save fetch-assets`: `--max-size`, `--concurrency`,
  `--per-host`, `--retries`, `--timeout`. `--backup-dir` is as in `cloud`.
- A URL that cannot be downloaded (dead link, timeout, too large, an HTML page)
  is listed as `failed: <url>: <reason>` and stays unchanged in the save; the
  rest is still moved. Exit status 0 when everything selected was moved or there
  was nothing to do, 2 when a download failed, 1 on errors (including a failed
  upload; the previous index is then in the backup the message names).
- There is no `--json` output.

Manual check (needs the real Steam Cloud, not part of the test suite): in a
throwaway project with a small save that uses two or three imgur images, run
`ttskit save rehost save.json --dry-run`, then the real run, open the new save in
the game and check that the images load, run `ttskit cloud status` and look at
`ttskit.manifest.json`; then run the command again on the new save, which must say
`Nothing to rehost`.

## MCP server

`ttskit mcp` runs a [Model Context Protocol](https://modelcontextprotocol.io/) server on stdio, so an AI assistant can work with the running game (read scripts, run Lua, push an edited tree with Save & Play, read the errors) and with the files of a mod project (inspect, lint, diff and check saves, rewrite URLs, build images, and manage the Steam Cloud). The editor tools are a client of the same External Editor API as the `editor` commands, with the same safeguards. The other tools run the same code as the matching CLI commands and return the JSON they print with `--json`.

| Tool | What it does |
| --- | --- |
| `get_status` | Whether the game answers, the loaded save, ports and root |
| `get_scripts` | Summary of all objects with a script or UI; with `guids`, their full text |
| `execute_lua` | Run Lua in Global or an object and return prints and Lua errors |
| `send_custom_message` | Send a JSON object to the game's `onExternalMessage` |
| `get_console` | Read buffered `print` output, Lua errors, custom messages, saves and loads |
| `save_and_play` | Send the scripts and UI of an unpacked tree and reload the game (plan first, then confirm) |
| `save_scan` | List the asset URLs of a save, grouped by host (`cloud scan --json`) |
| `save_lint` | Check a save for common mistakes (`save lint --json`) |
| `save_diff` | Compare two saves semantically (`save diff --json`) |
| `save_check_urls` | Request every asset URL of a save and report dead ones (`save check-urls --json`; uses the network) |
| `manifest_show` | Show the asset manifest (`cloud manifest --json`) |
| `save_rewrite_urls_plan` / `_apply` | Replace asset URLs in a save from a mapping or manifest (`save rewrite-urls`) |
| `image_check` | Report images that are too large for the game (`image check --json`) |
| `image_build_plan` / `_apply` | Build the images of the project config's pipeline (`image build`) |
| `cloud_list` | List the Steam Cloud files, optionally of one folder (`cloud list --json`) |
| `cloud_status` | Compare a local directory with a cloud folder (`cloud status --json`) |
| `cloud_upload_plan` / `_apply` | Upload one local file to the cloud and record it in the manifest (`cloud upload`) |
| `cloud_sync_push_plan` / `_apply` | Upload the new local files of the project config's folder; with `prune`, also replace changed and delete cloud-only files (`cloud sync push [--prune]`) |
| `cloud_sync_pull_plan` / `_apply` | Download the cloud folder's files into the project's local directory; with `prune`, also recycle local-only files (`cloud sync pull [--prune]`) |

Which tools exist depends on the build:

- The `image_*` tools exist only in builds with the `image` feature, and the `cloud_*` tools only in builds with the `steam` feature; other builds do not list them. The editor and save tools are in every build.
- The cloud tools open Steam for each call, never at server start: Steam must be running and logged in, otherwise the call fails with the reason. Cloud calls run one at a time. They read the project config (`ttskit.toml` in the root by default) like the CLI. The index backups of upload and push go to the CLI's backup directory (`TTSKIT_BACKUP_DIR`, else the default); the assistant cannot choose it.
- `image_check` findings and the files it could not read (`errors`) are in the JSON result, not tool errors, and `cloud_status` "not in sync" is a normal result (`in_sync` in the JSON).

### Plan and apply

Every tool that changes something works in two steps, so that nothing happens without a look at the plan:

1. The `*_plan` tool (for Save & Play: the first `save_and_play` call) shows what would happen, as the CLI's dry-run JSON, and returns a `fingerprint`. It changes nothing.
2. The `*_apply` tool takes the same arguments plus that `fingerprint` and `confirm: true`. It computes the plan again from the current state and only acts if the fingerprint still matches. Otherwise it refuses, with nothing done, and returns the fresh plan; so a file or cloud change in between, a missing `confirm` or a wrong fingerprint all stop it. Plans with conflicts (sync) are never applied.

Overwriting an existing output (`out`, `in_place`, a cloud file of the same name) is only possible through apply. Sync push and pull only add files unless `prune: true` is given to the plan and again to the apply: push then also replaces changed cloud files and deletes cloud files that are not local (the cloud index is backed up first), and pull moves local files that are not in the cloud folder to the recycle bin. The plan lists every such step and the fingerprint covers `prune`, so a plan made without it cannot be applied with it, or the other way round. Deleting cloud files directly (`cloud delete`) and `image build --prune` are not offered.

Safeguards and limits:

- `save_and_play` never sends on the first call: it returns the diff and a fingerprint of the plan. Only a second call with `confirm` set to that fingerprint sends, and only if the plan is still the same. Deleting a script or UI that the tree lacks also needs `allow_delete`.
- Every path argument of every tool must be inside the server's root: the working directory, or `--root DIR`. Paths with `..`, absolute paths outside the root and symlinks leading out of it are refused. Only the `*_apply` tools write files (and `save_and_play` sends to the game).
- The server holds the editor port (39998) for its whole life and buffers what the game sends, also between tool calls (1000 events and 4 MiB by default; `--console-buffer` changes the count). Every event has a sequence number, so `get_console` with `since` returns only what is new. While the server runs, `ttskit watch`, `ttskit editor console` and other editor integrations cannot receive from the game; if one of them already listens, the server still starts, but the editor tools fail with that reason (`get_status` shows `editorAvailable: false`) while the save tools keep working. See [docs/editor-api.md](docs/editor-api.md#ttskit-mcp) for integrations that listen on the wildcard address.
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

### Building deck sheets

```
ttskit image deck build cards/ --out-dir deck
ttskit image deck build front/*.png --hidden back.png --name mydeck --out-dir deck
ttskit image deck build --list cards.txt --fit contain --max-sheet-side 4096 --out-dir deck
ttskit image deck build cards/ --no-hidden-slot --dry-run
```

`image deck build [CARD...] --out-dir DIR` assembles single card images into the
grid sheets of a custom deck and writes a description of them. A CARD is a file
or a directory; a directory gives its image files (not those of subdirectories)
in natural order (`card2` before `card10`). Files keep the order you give them,
and a file may repeat, each time in its own slot. `--list FILE` adds more: one
path per line, blank lines and lines starting with `#` ignored, relative paths
relative to the list, after the CARD arguments. Output goes to `NAME.png` (or
`NAME_1.png`, `NAME_2.png`, ... for several sheets) and `NAME.json`; `--name`
defaults to `deck`, `--to jpg` (with `--quality`) writes JPEG.

- **Slots and sheets.** A sheet has at most 10 columns and 7 rows. Slots are
  numbered from 0, left to right and top to bottom, and card `n` of a sheet is
  in slot `n`. The last slot is reserved for the hidden-card image (`--hidden
  FILE`, or the `--fill` colour, default `808080`), so a 10x7 sheet holds **69**
  cards; `--no-hidden-slot` gives **70** and sets `back_is_hidden`. More cards go
  on further sheets, each full except the last. Why the last slot is special is
  in [docs/deck-sheets.md](docs/deck-sheets.md).
- **Grid.** The smallest roughly square grid that fits, but never below 2x2
  (the in-game dialog takes no less), and 10x7 for every sheet but the last.
  `--columns N` and `--rows N` fix a dimension (1 to 10 and 1 to 7); the last
  sheet keeps it and takes the smallest value of the other. With both, every
  sheet has that grid.
- **Card size and fit.** All cards have the size of the first card, or
  `--card-size WxH`. A card of another size is an error that lists every such
  file, unless `--fit` says what to do: `contain` (scale to fit inside, centred
  on the fill colour), `cover` (scale to cover, crop the centre) or `stretch`.
  A card that already has the cell size is copied as it is; resampling is
  Lanczos 3, premultiplied for transparency.
- **Size limit.** `--max-sheet-side N` scales the cards down evenly until no
  sheet side exceeds N pixels (never up; each card is resampled once, from the
  original). Without it a warning is given for a sheet side over 4096 px, the
  limit of `image check`.
- **Pixels.** PNG sheets are RGB unless a pixel is transparent. A JPEG sheet
  flattens transparency onto `--fill`. 16-bit input is reduced to 8-bit, the
  colour profile and EXIF data are dropped (an orientation is applied first), an
  animated image gives its first frame. PNG, JPEG and WebP are read.
- **Output rules** as in the other image commands: existing files are never
  overwritten without `--yes`, the run is refused before anything is written if
  outputs collide or one is an input, `--dry-run` builds in memory and writes
  nothing, files are written atomically, and the same input and options give the
  same bytes. Exit status 1 on any error.

The description `NAME.json` (also under `description` in the `--json` report,
next to `dry_run`, `files`, `notes` and `errors`) says where everything is:

```
{
  "format_version": 1,
  "name": "deck",
  "card_width": 300, "card_height": 420,
  "back_is_hidden": false,
  "sheets": [
    { "file": "deck_1.png", "columns": 10, "rows": 7, "width": 3000, "height": 2940,
      "card_count": 69, "hidden_slot": 69,
      "cards": [ { "index": 0, "slot": 0, "column": 0, "row": 0, "source": "cards/a.png" } ] }
  ]
}
```

`hidden_slot` is `null` with `--no-hidden-slot`; `source` is the path as given
(or, from a list, joined to the list's directory), with `/` separators.
Notes have stable codes: `scaled-down`, `alpha-flattened`, `reduced-to-8-bit`,
`orientation-applied`, `icc-dropped`, `exif-dropped`, and the warnings
`sheet-too-large` and `animated-first-frame`.

### Splitting deck sheets

```
ttskit image deck split deck.png --description deck.json --out-dir cards
ttskit image deck split sheet.png --columns 10 --rows 7 --count 40 --out-dir cards
ttskit image deck split sheet.jpg --save save.json --deck-id 1 --to jpg --out-dir cards
ttskit image deck split sheet.png --columns 5 --rows 2 --no-hidden-slot --dry-run
```

`image deck split SHEET --out-dir DIR` is the reverse of `image deck build`: it
cuts one sheet (PNG, JPEG or WebP) into single card images. The grid comes from
exactly one of three places:

- `--columns N --rows N` (1 to 10 and 1 to 7). The last slot is assumed to be
  the hidden-card slot, as in `image deck build`; say `--no-hidden-slot` if all
  slots are cards.
- `--description FILE`, the `NAME.json` of `image deck build`. The sheet is
  found by its file name among the `sheets` of the description (an exact,
  case-sensitive match; the error lists the names that exist). Columns, rows,
  the card count (the default of `--count`) and the hidden-card slot (`null`:
  none) are taken from it.
- `--save FILE --deck-id ID`, a save file. Every `CustomDeck` entry with that
  key, in objects at any depth, gives `NumWidth` and `NumHeight`; `BackIsHidden:
  true` means there is no hidden-card slot (the back is a separate image and all
  slots are cards), `false` that the last slot is the hidden-card slot (see
  [docs/deck-sheets.md](docs/deck-sheets.md)). The entries must agree with each
  other, and an unknown ID is an error that lists the IDs the save has.

Slots are numbered as in `build`. Card slots are written as `NAME_NN.png` with
the slot number padded to at least 2 digits (`_00` to `_68` for a 10x7 sheet; the
width depends on the grid, never on `--count`), the hidden-card slot as
`NAME_hidden.png`. `NAME` is the file name of the sheet without extension, or
`--name`. `--count N` writes only the first N cards (an error if N is 0 or more
than the card slots), `--skip-hidden` leaves the hidden-card slot out, and
`--no-hidden-slot` treats the last slot as a card whatever the description or
save says. A sheet whose size does not divide evenly into the grid is an error
that names both, and nothing is written.

Cards are written as PNG whatever the sheet's format (`--to jpg` with `--quality`
for JPEG, flattening transparency onto `--fill`, default `808080`). A PNG card
keeps the pixels of the sheet exactly, including 16-bit channels and the colour
type, so `build` followed by `split` gives back the original card pixels
(compare the decoded pixels, not the file bytes: files are re-encoded). The
colour profile and EXIF data are dropped, an EXIF orientation is applied first,
and an animated image gives its first frame. Output rules are those of `build`:
existing files need `--yes`, the whole run is refused before anything is
written, `--dry-run` writes nothing, output is deterministic, exit status 1 on
any error. `--json` prints one object with `dry_run`, `sheet`, `columns`,
`rows`, `cell_width`, `cell_height`, `grid_source` (`flags`, `description` or
`save`), `files` (`path`, `slot`, `column`, `row`, `hidden`, `width`, `height`,
`bytes`, `overwrote`, `written`), `notes` and `errors`.

### Deck objects

```
ttskit image deck build cards/ --out-dir assets
ttskit cloud sync push                       # upload the sheets, fills the manifest
ttskit image deck object assets/deck.json --back-url https://example.invalid/back/ --out deck_object.json
ttskit image deck object assets/deck.json --face-url URL --back-url URL --meta meta.json --out deck_object.json
ttskit image deck object assets/deck.json --unique-backs --back-url B1 --back-url B2 --out deck_object.json
```

`image deck object DESCRIPTION --out FILE` turns the `NAME.json` of `image deck
build` into a saved-object file (`{"ObjectStates": [...]}`, written like the
game writes JSON), so a deck does not have to be set up in the custom deck
dialog. Two or more cards give a `DeckCustom` with one `CustomDeck` entry per
sheet (keys `1`, `2`, ...), `DeckIDs` and one `Card` per card in
`ContainedObjects`; a card id is the sheet's key times 100 plus the slot. A deck
with one card gives a single `CardCustom`, as the game does.

- **URLs.** `--face-url` (once per sheet, in order) and `--back-url` are URLs, or
  the local path of an uploaded file. Paths, and the face sheets when
  `--face-url` is left out (the sheet files named in the description, relative
  to its folder), are looked up in the project manifest of `--config` (default
  `ttskit.toml` in the current directory). A file that changed after its upload
  is refused, so the object never points at an outdated sheet.
- **Backs.** One `--back-url` is shared by all sheets. `--unique-backs` sets
  `UniqueBack` and takes one back sheet per face sheet, in order.
  `BackIsHidden` follows the description (`image deck build --no-hidden-slot`
  gives `true`).
- **Text.** Nicknames are empty by default. `--meta FILE` sets `Nickname`,
  `Description` and `GMNotes` of the deck and of each card:

  ```json
  {
    "deck": { "nickname": "My deck", "description": "...", "gm_notes": "..." },
    "cards": [ { "nickname": "Ace" }, null, { "gm_notes": "..." } ]
  }
  ```

  `cards` is a list in deck order (`null` for none) or an object keyed by a
  card's `source` in the description or by its file name (`"ace.png": {...}`; an
  ambiguous or unknown name is an error). All keys are optional.
  `--nicknames-from-files` names the cards without a nickname after their source
  file.
- GUIDs are derived from the content, so the output is identical between runs.
  An existing `--out` file needs `--yes`, `--dry-run` writes nothing, `--json`
  prints one object, exit status 1 on any error.

The field set was compared with a deck made in the game; see
[docs/deck-sheets.md](docs/deck-sheets.md).

Which formats, animated images, 16-bit channels and colour profiles the image
commands accept or produce follows what the game itself accepts; see
[docs/image-formats.md](docs/image-formats.md).

### Image pipeline (image build)

```
ttskit image build --dry-run     # show what would be built, write nothing
ttskit image build               # build what changed
ttskit cloud sync push           # upload the changed outputs
```

`image build` turns the source art of a project into the files that go to the
cloud, as the rules in `ttskit.toml` say, so the whole flow is one command and
the outputs can be rebuilt at any time. A project could look like this:

```
my-game/
  ttskit.toml
  ttskit-image.lock.json    written by image build
  art/                      sources, never modified
    boards/
      main.png
      side/extra.png
    cards/
      ace.png
      king.png
      hidden.png
    icons/
      logo.png
  assets/                   outputs, what `cloud sync push` uploads
    Boards/
      main.png
      side/extra.png
    Cards/
      base.png
      base.json
    Icons/
      logo.jpg
```

with this `ttskit.toml`:

```toml
[cloud]
folder = "MyGame"

[local]
dir = "assets"
ignore = ["*.json"]          # keep the deck descriptions out of the upload

[[image.pipeline]]           # boards: at most 4096 px, then smaller files
src = "art/boards/**/*.png"
out = "assets/Boards"
steps = [
  { op = "resize", max-side = 4096 },
  { op = "optimize", level = 4 },
]

[[image.pipeline]]           # the cards of the folder become one deck
src = "art/cards/[!h]*.png"
out = "assets/Cards"
steps = [{ op = "optimize" }]
deck = { name = "base", hidden = "art/cards/hidden.png", columns = 5, rows = 2 }

[[image.pipeline]]           # icons as JPEG, flattened onto white
src = "art/icons/*.png"
out = "assets/Icons"
steps = [{ op = "convert", to = "jpg", quality = 90, background = "#ffffff" }]
```

`[cloud]` and `[local]` are the sections of the cloud commands; `image build`
reads only `[[image.pipeline]]` and ignores the others. A rule has:

| key     | meaning |
|---------|---------|
| `src`   | A file or a glob (`*`, `**`, `?`, `[...]`, `{a,b}`) relative to the directory of `ttskit.toml`; matched ignoring case, `*` does not cross `/`. Required. |
| `out`   | The output directory, relative to the same directory; created as needed. Required. |
| `steps` | A list of steps, run in order on each source (on each sheet for a deck). Default: none, which copies the source unchanged. |
| `deck`  | Optional; makes all files of `src` one deck (see below). |

All keys are lower case with `-` (`max-side`). Unknown keys, a missing `src` or
`out`, and invalid values are refused with the key and its position, for
example `image.pipeline[2].steps[1].level`.

#### Steps

Every step is a table with an `op` and the options of the command of the same
name. Defaults are those of the command.

| `op`       | option | default | meaning |
|------------|--------|---------|---------|
| `resize`   | `max-side`, `width`, `height` or `scale` | none | Exactly one of them is required, as `--max-side`, `--width`, `--height`, `--scale` of `image resize`. |
|            | `allow-enlarge` | `false` | Allow making the image larger. |
|            | `filter` | `"lanczos3"` | `"lanczos3"`, `"catmull-rom"`, `"triangle"` or `"nearest"`. |
|            | `quality` | `85` | JPEG quality, 1 to 100, when the image is a JPEG. |
| `optimize` | `level` | `2` | 0 to 6; higher is smaller and slower. |
|            | `strip` | `"safe"` | `"safe"`, `"all"` or `"none"`. |
| `quantize` | `colors` | `256` | 2 to 256. Lossy. |
|            | `quality` | none | A range such as `"60-90"`, as `--quality` of `image quantize`. |
|            | `dither` | `1.0` | 0 to 1. |
|            | `level`, `strip` | `2`, `"safe"` | As in `optimize`. |
|            | `engine` | `"builtin"` | `"builtin"` or `"pngquant"`. |
|            | `program` | `pngquant` | The pngquant program; implies `engine = "pngquant"`. A path with a directory is relative to `ttskit.toml`, a bare name is looked up on `PATH`. |
| `convert`  | `to` | none | `"png"` or `"jpg"`. Required. |
|            | `quality` | `85` | JPEG quality, 1 to 100; only with `to = "jpg"`. |
|            | `background` | none | `#RRGGBB`, `#RGB`, `white` or `black`; transparency is flattened onto it. Without it, an image with transparent pixels cannot become a JPEG. Only with `to = "jpg"`. |
|            | `keep-metadata` | `false` | Keep the colour profile and EXIF data. |

The steps behave exactly like the commands (same notes, same limits); see
their sections above. A step that has nothing to do, such as `optimize` on a
JPEG, leaves the file as it is and adds a note.

#### Decks

`deck = { name = "base", ... }` makes the files matched by `src` (in natural
order of their path) the cards of one deck, as `image deck build` does, instead
of one output per source. The steps then run on each sheet. A `convert` step is
not allowed in such a rule, because the deck description names the sheet files;
use `to` of the deck instead.

| key              | default | meaning |
|------------------|---------|---------|
| `name`           | required | Stem of the output files; a plain name without a path. |
| `hidden`         | none | The hidden-card image, relative to `ttskit.toml`. It is not a card, so keep it out of `src`. |
| `columns`, `rows` | smallest roughly square grid | 1 to 10 and 1 to 7. |
| `card-size`      | size of the first card | `"WxH"`. |
| `fit`            | none | `"contain"`, `"cover"` or `"stretch"` for cards of another size. |
| `hidden-slot`    | `true` | `false` makes the last slot a card too. |
| `fill`           | `"808080"` | Colour of the empty hidden slot and of `contain` borders. |
| `max-sheet-side` | none | Scale cards down until no sheet side is longer. |
| `to`, `quality`  | `"png"`, `85` | Sheet format; `quality` only for `"jpg"`. |

#### Where outputs go

A source maps to `out` plus its path relative to the fixed part of `src`, the
leading directories without glob characters. `art/boards/side/extra.png` under
`src = "art/boards/**/*.png"` becomes `assets/Boards/side/extra.png`; for a
`src` that names one file, the file name is used. A `convert` step changes the
extension (`logo.png` becomes `logo.jpg`). A deck writes `NAME.png`, or
`NAME_1.png`, `NAME_2.png`, ... for several sheets (`.jpg` with `to = "jpg"`),
and the description `NAME.json` of `image deck build`, which `image deck object`
takes.

If two sources would write one output, or an output is a source or the config
or lock file, the build stops before anything is written and lists every
conflict. So does a rule that cannot be planned, such as a bad deck grid or a
missing pngquant. Outputs that already exist are overwritten without asking;
sources are never changed.

#### Incremental builds

`ttskit-image.lock.json`, next to `ttskit.toml`, records for each output the
hash of its inputs, the options, the version of ttskit, and the hash of the file
that was written. An output is rebuilt when:

- it is new, or its file is missing (deleted by hand);
- the file differs from what was built (edited by hand);
- an input file changed, or the list of cards of a deck;
- a step or deck option of its rule changed;
- the ttskit version changed, since a newer ttskit may compress differently;
- for a `pngquant` step, the pngquant version changed.

Everything else is reported as up to date and not touched, so unchanged
outputs keep identical bytes and `cloud sync push` does not upload them again.
Commit the lock file if you commit the outputs, so a fresh checkout does not
rebuild everything. The file is plain, sorted JSON; deleting it just rebuilds
everything. It is also written after a partly failed build, so what did succeed
is recorded.

#### Stale outputs and `--prune`

An output that an earlier build made and no rule makes any more (a rule was
changed or deleted, a source was removed) is stale. `image build` lists it and
leaves it alone. `--prune --yes` deletes the stale outputs that are still
exactly what ttskit wrote; one that was changed by hand since is kept and
forgotten, one that is already gone is just forgotten, and no directory is
removed. Files that ttskit did not write are never touched. `--prune` without
`--yes` is refused before anything is built; with `--dry-run` it only shows the
plan, which already lists the stale files.

#### Options

- `--config PATH`: the project config (default `ttskit.toml`); its directory
  is the base of all paths in it.
- `--dry-run`: print the plan and write nothing, not even the lock file.
- `--prune --yes`: also delete stale outputs, as above.
- `--json`: print one object with `dry_run`, `plan` (`jobs`, `stale`, `notes`),
  `build` (per job its result and the outputs written with their sizes and
  notes; `null` for a dry run) and `prune` (`null` unless pruned).

The plan has one line per job that will be built with the reason (`new`,
`output missing`, `output changed`, `version changed`, `inputs changed`,
`options changed`), a count of the up-to-date ones, and the stale outputs. The
exit status is 1 if a job failed or a file could not be deleted; a job that
fails does not stop the others.

The description of a deck lands in `out` next to its sheets, so `cloud sync
push` would upload it. Add a pattern such as `*.json` to `[local] ignore`, as
in the example, if you do not want that; the file is still built.

## License

[MIT](LICENSE)

The Lua syntax check of `save lint` uses the [full_moon](https://crates.io/crates/full_moon) parser, which is licensed under MPL-2.0; its source is unchanged and available from its crate. The other dependencies are permissively licensed.
