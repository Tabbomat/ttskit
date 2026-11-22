# Unpacked save layout

This document specifies the directory layout that `ttskit save unpack` writes and
`ttskit save pack` reads. It is the contract for the unpack and pack
implementations (#5, #6, #7), for safe output handling (#8), and for the later
bundling (#9) and asset placeholder (#47) features.

ttskit is an unofficial tool and is not affiliated with Berserk Games.

## Goals

1. **Stable git diffs.** A small change in the game gives a small, local diff.
   Files that did not change are written byte for byte the same.
2. **Readable.** Scripts, UI and script state are plain files that an editor
   understands. Directory names say what the object is.
3. **Lossless.** Every field of the save ends up in exactly one file. Key order
   and number text are preserved (see the save model in `ttskit-core`).
4. **Exact round trip on request.** `pack --keep-eol` gives a byte-identical
   save for saves written by Tabletop Simulator (limits below).

## Sample tree

```text
MySave/
  .ttskit/
    unpack.json                 metadata (format version, created files, line endings)
  save.json                     global data, with markers for extracted fields
  script.lua                    global LuaScript
  ui.xml                        global XmlUI
  state.json                    global LuaScriptState (or state.txt)
  notebook/
    0.Rules.txt                 TabStates["0"].body
    1.Setup.txt
  objects/
    index.json                  order of ObjectStates
    Board.f00d01/
      object.json
      script.lua
      ui.xml
    Outer bag.bag001/
      object.json
      contents/
        index.json              order of ContainedObjects
        Middle bag.bag002/
          object.json
          contents/
            index.json
            Leaf token.leaf01/
              object.json
    Top level deck.deck01/
      object.json
      contents/
        index.json
        Card 100.c01000/
          object.json
        Card 101.c01001/
          object.json
    State one.sta001/
      object.json
      script.lua
      state.json
      states/
        index.json              keys and order of States
        2.State two.sta002/
          object.json
          script.lua
          state.json
        3.State three.sta003/
          object.json
          script.lua
    Lamp.a1b2c3/
      object.json
      children/
        index.json              order of ChildObjects
        Lamp shade.d4e5f6/
          object.json
```

## Directories and files

The root holds the global part of the save; every object has a directory of its
own. The same file names are used at both levels:

| File or directory | Field | Present when |
|---|---|---|
| `save.json` (root) / `object.json` (object) | everything not extracted | always |
| `script.lua` | `LuaScript` | non-empty string |
| `ui.xml` | `XmlUI` | non-empty string |
| `state.json` or `state.txt` | `LuaScriptState` | non-empty string (see [Script state](#script-state)) |
| `objects/` (root only) | `ObjectStates` | non-empty array of objects |
| `contents/` | `ContainedObjects` | non-empty array of objects |
| `states/` | `States` | non-empty map of objects |
| `children/` | `ChildObjects` | non-empty array of objects |
| `notebook/` (root only) | `body` of each `TabStates` entry | non-empty string |

Every object gets its own directory, at any depth, including the cards of a
deck. This keeps the rules uniform; a change to one card touches one file.

A field is extracted only when it has the type and content in the table. Any
other value (`""`, `[]`, `{}`, `null`, a number, a wrong type) stays inline in
`save.json` or `object.json`. If any entry of `ObjectStates`,
`ContainedObjects`, `ChildObjects` or `States` is not a JSON object, the whole
field stays inline, so nothing is lost.

## Markers: keeping the key order

An extracted field keeps its key at its original position in `save.json` or
`object.json`. Its value is replaced by a marker string that names the file or
directory, relative to the JSON file, with `/` as separator:

```json
{
  "GUID": "sta001",
  "Name": "Custom_Model",
  "Nickname": "State one",
  "Locked": false,
  "LuaScript": "file:script.lua",
  "LuaScriptState": "file:state.json",
  "XmlUI": "",
  "States": "dir:states"
}
```

| Marker | Fields |
|---|---|
| `file:script.lua` | `LuaScript` |
| `file:ui.xml` | `XmlUI` |
| `file:state.json`, `file:state.txt` | `LuaScriptState` |
| `dir:objects` | `ObjectStates` |
| `dir:contents` | `ContainedObjects` |
| `dir:states` | `States` |
| `dir:children` | `ChildObjects` |
| `file:notebook/<name>.txt` | `body` inside a `TabStates` entry |

Because unpack extracts every non-empty string in these fields, an inline
non-empty string in them never comes from unpack. On pack, a value of one of
these fields that is a marker of the matching kind is resolved; any other value
is taken literally. A user may therefore also write a short script inline in
`object.json`.

Markers are recognized only in the fields listed above, never elsewhere. A mod's
own data that happens to contain the text `file:script.lua` is left alone.

Marker paths must be exactly the names in the table. Pack rejects any other
path (absolute, `..`, another file name).

## Order: `index.json`

`objects/`, `contents/` and `children/` each contain an `index.json` that lists
the child directory names in save order:

```json
[
  "Board.f00d01",
  "Outer bag.bag001",
  "Top level deck.deck01"
]
```

`states/index.json` lists the state keys with their directory, in the order of
the `States` map in the save:

```json
[
  { "key": "2", "dir": "2.State two.sta002" },
  { "key": "3", "dir": "3.State three.sta003" }
]
```

Reordering objects or adding one is a one-line change in `index.json`.

The notebook needs no index: the `TabStates` map stays in `save.json`, only each
`body` is replaced by a marker.

## Directory names

An object directory is named `<name>.<GUID>`:

- `<name>` is the `Nickname`; if it is missing or empty, the `Name`.
- No usable name: the directory is just `<GUID>`.
- No name and no GUID: `object`.
- A state directory is prefixed with the state key: `<key>.<name>.<GUID>`.
- A notebook file is named `<key>.<title>.txt` from the `TabStates` key and the
  entry's `title`; without a title, `<key>.txt`.

Sanitizing, applied in this order:

1. The characters `< > : " / \ | ? *` and control characters (U+0000 to U+001F,
   U+007F) become `_`. Other non-ASCII characters are kept (`Tüte 日本語 🎲`).
2. The `<name>` part (or `<title>`) is truncated to 24 characters (Unicode
   scalar values, never inside a character). The GUID and key are never
   truncated.
3. Trailing dots and spaces of the whole name are removed.
4. If the part before the first dot is a Windows reserved device name (`CON`,
   `PRN`, `AUX`, `NUL`, `COM1` to `COM9`, `LPT1` to `LPT9`, case-insensitive),
   `_` is appended to that part: `CON_.a1b2c3`.
5. Collisions within one directory (compared case-insensitively, including the
   reserved name `index.json`) get a suffix `-2`, `-3`, ... in save order:
   `Card.c00001`, `Card.c00001-2`. Duplicate GUIDs are common in real saves,
   for example cards copied in a deck.

The directory name is only a label. Pack takes the identity of an object from
`index.json` and the data from `object.json`; it never parses directory names.
Renaming a directory together with its `index.json` entry is allowed.

A nickname change in the game renames the directory on the next unpack. Git
shows this as a rename when the content is otherwise similar.

## File formats and line endings

All files are UTF-8 without a byte order mark.

**Line endings on disk** are always the native ones of the operating system
(CRLF on Windows, LF elsewhere), in every file ttskit writes: scripts, UI,
script state, notebook pages and JSON files.

**Scripts, UI and notebook pages** contain the field's text with only the line
endings converted. Nothing else changes; no trailing newline is added.

**JSON files** (`save.json`, `object.json`, `index.json`, prettified
`state.json`, `.ttskit/unpack.json`) use the structure of Tabletop Simulator's
own output: two spaces of indentation, one key per line, `": "` after keys,
`{}` and `[]` for empty values, the same string escaping, and numbers with their
original text. They end with a trailing newline.

Inline string values (for example a `Description` that contains `\r\n`) stay
escaped exactly as in the save; unpack does not touch their line endings.

**Git:** to keep a repository usable on Windows and other systems, add a
`.gitattributes` that lets git normalize these files, for example:

```text
*.lua  text
*.xml  text
*.json text
*.txt  text
```

ttskit does not write this file.

### Script state

`LuaScriptState` is a string, usually compact JSON written by a mod's `onSave`.

- If the string is valid JSON and compacting the parsed value gives back the
  exact original string, it is written prettified as `state.json` and recorded
  as prettified in the metadata. Pack compacts it again.
- If it is valid JSON but not in that canonical compact form, it is written
  verbatim as `state.json`.
- Otherwise it is written verbatim as `state.txt`.

## Pack

Pack reads the tree, resolves markers, and writes the save.

**Default output: LF.** Everything is written with LF line endings: the JSON
layout of the save, the extracted text fields (scripts, UI, state, notebook
pages) and every inline string value, where each `\r\n` becomes `\n` (a lone
`\r` is kept). A manual check on Windows confirmed that Tabletop Simulator
loads such a save, shows scripts correctly in the in-game editor and runs them;
when the game saves it again it writes the JSON layout with CRLF but keeps LF
inside the strings.

Tabletop Simulator appears to use the operating system's default line ending
for the JSON layout (CRLF on Windows, LF on other systems); only the Windows
behaviour has been observed.

**`--keep-eol`:** keeps the line endings of the original save:

- The JSON layout gets the style recorded at unpack.
- Each extracted text field gets the style recorded for it at unpack.
- Inline string values are written exactly as they are.
- Anything without a record (a newly added script, a tree without metadata)
  gets the native line ending of the system pack runs on.

With it, unpack followed by pack reproduces a save written by Tabletop
Simulator byte for byte, with these limits:

- One style is recorded per field (`crlf` or `lf`). A field with mixed line
  endings is written in its majority style and is not byte-exact; unpack lists
  such fields.
- The limits of the save model apply (duplicate keys, input written by
  third-party tools; see `ttskit_core::save`).

Without `--keep-eol`, the packed save equals the original after line endings are
normalized. In both modes, unpack, pack and unpack again gives an identical tree.

## Metadata: `.ttskit/unpack.json`

```json
{
  "format": 1,
  "ttskit": "0.1.1",
  "layout_eol": "crlf",
  "files": [
    "save.json",
    "script.lua",
    "objects/index.json",
    "objects/Board.f00d01/object.json"
  ],
  "eol": {
    "script.lua": "crlf",
    "objects/Board.f00d01/script.lua": "lf"
  },
  "mixed_eol": [],
  "pretty_state": [
    "objects/State one.sta001/state.json"
  ]
}
```

- `format`: version of this layout. Pack refuses a format it does not know.
- `ttskit`: version that wrote the tree (informational).
- `layout_eol`: line ending style of the save's JSON layout. Used only by
  `--keep-eol`.
- `files`: every file unpack created, relative to the root, `/` separated. Used
  only by `unpack --clean`.
- `eol`: original line ending style of each extracted text file. Used only by
  `--keep-eol`.
- `mixed_eol`: files whose field had mixed line endings.
- `pretty_state`: state files that pack must compact.

Pack works without the metadata file: it then treats no state file as
prettified and `--keep-eol` uses native line endings for the layout and every
extracted field. Paths in the metadata
that no longer exist are ignored.

## Editing rules

| Situation | Pack behaviour |
|---|---|
| A file was edited | Its content is used. |
| A marker names a file or directory that does not exist | The key is removed from the object. |
| `script.lua`, `ui.xml`, `state.json`/`state.txt`, `contents/`, `states/` or `children/` exists, and the object has no such key | The key is added at the end of the object. |
| Such a file or directory exists, and the key holds an empty value (`""`, `[]` or `{}`, as unpack leaves it) | The value is replaced in place by the file or directory. |
| Such a file or directory exists, but the key holds another non-marker value or a marker for another file | Error naming both. |
| `state.json` and `state.txt` both exist | Error. |
| A directory in `objects/`, `contents/`, `states/` or `children/` is not listed in its `index.json` | Error naming the directory. |
| `index.json` lists a directory that does not exist | Error naming `index.json` and the entry. |
| A prettified `state.json` was edited | It is compacted. If it is no longer valid JSON: error. |
| A JSON file is malformed | Error with the path and position. |
| Unknown files (no marker, not in an index) | Ignored with a warning. Names starting with `.` (`.git`, `.gitattributes`, `.ttskit`) are ignored silently. |
| A text file starts with a UTF-8 byte order mark | The mark is dropped. |
| A text file is not valid UTF-8 | Error. |

Errors abort pack before anything is written.

## Safe output handling

Neither command silently destroys data, and output paths are always explicit:
the save and the output are never searched for in a game directory.

`ttskit save unpack SAVE -o DIR`

| Situation | Behaviour |
|---|---|
| `DIR` is missing or empty | The tree is written. |
| `DIR` is not empty | Refused, nothing is written. |
| `DIR` is not empty and `--force` (alias `--yes`) | Files unpack creates are written, overwriting existing ones. Every other file is left alone. |
| `--force --clean` | Also deletes the files listed in `files` of the previous `.ttskit/unpack.json` that this unpack does not create, and directories that this leaves empty. Nothing else is ever deleted. |
| `--clean` without `--force` | Usage error. |
| `--dry-run` | Prints `create`, `change` and `remove` lines and a summary, writes and deletes nothing. The same refusals apply, so a dry run predicts the real run. |

- `--clean` removes only regular files named in the previous metadata. Entries
  that are absolute, contain `..` or a backslash or a drive colon, or an
  unreadable or malformed metadata file make `--clean` fail before anything is
  written, so a damaged file can never point outside `DIR`. Without a metadata
  file `--clean` removes nothing.
- The metadata is rewritten by every unpack. Files left over from an earlier
  unpack that ran without `--clean` are no longer tracked, so a later `--clean`
  does not touch them.
- A tracked file that was edited is still removed by `--clean`; use
  `--dry-run` to review the list.

`ttskit save pack DIR -o FILE`

| Situation | Behaviour |
|---|---|
| `FILE` does not exist | It is created (with missing parent directories). |
| `FILE` exists | Refused, nothing is written. |
| `FILE` exists and `--force` (alias `--yes`) | Overwritten through a temporary file next to it, so a failure leaves the old file. |
| `--dry-run` | Packs in memory (so errors in the tree are reported), prints whether `FILE` would be created or changed, writes nothing. |

An output path that exists but is not a regular file is always an error.

Unpack never deletes files it did not create.

## Bundling

Shared Lua and XML modules live in their own directories and are inlined by
`pack`. The unpacked tree always keeps the unbundled form.

`ttskit save pack DIR -o FILE --include LIB_DIR [--include LIB_DIR2 ...]`

Without `--include`, scripts and UI are packed exactly as they are (a
`require` stays a `require`). With it, every `script.lua` and `ui.xml` is
bundled: the global ones and those of every object at any depth. Inline
scripts written directly in `object.json` are not bundled. Include
directories are searched in the order given; the first match wins.

### Lua

`require("Name")` resolves to `<lib-dir>/Name.lua`, or `Name.ttslua` if there
is no `.lua` file. Dots and `/` both separate directories:
`require("util.math")` and `require("util/math")` find `util/math.lua`.

- Only `require` calls with a literal string argument are followed
  (`require("x")`, `require 'x'`, `require([[x]])`). Calls in comments and
  strings, `obj.require(...)`, `obj:require(...)` and `require(name)` are
  ignored.
- Names that could leave the include directory (`..`, a leading `/`, empty
  parts) are an error.
- A script with no `require` calls is left byte for byte as it is.
- A script that already contains `__bundle_register` is considered bundled
  and left alone, so packing a tree that holds a bundled script does not
  bundle twice.

The result uses the module registry of the Tabletop Simulator editor plugins
(Atom and VS Code): a prelude, one `__bundle_register("Name", function(...)
... end)` per module, the original script as `__root`, and
`return __bundle_require("__root")`. At runtime each module runs once on its
first `require`, and the result is cached. Cyclic requires are legal, as in
plain Lua: the second `require` inside the cycle returns `nil`. If one module
is required under two spellings (`util.math` and `util/math`) the file is
registered once and the other spelling is a one-line alias. The script's
globals (`onLoad`, `onSave`, ...) still work: the game calls them as before.
Line numbers in Lua error messages refer to the bundle, not to the source
files.

Every object that requires a module gets its own copy, because scripts of
different objects are isolated in the game. Packing never reorders or
changes anything else.

### XML

`<Include src="Name"/>` (or `<Include src="Name"></Include>`) is replaced by
the content of `<lib-dir>/Name.xml`; `/` separates directories. Includes
inside included files are expanded too. Includes inside XML comments and
CDATA sections are left alone. An include cycle is an error that lists the
chain.

### Errors

Pack stops before writing anything. A module that cannot be found is
reported with the module name and the file that requires it (the script or UI
file for a direct require, the module file for a nested one), together with
the directories searched. A missing include directory, a name that is not
valid, an unreadable file or invalid UTF-8 are errors as well.

### How bundled output differs from the unpacked source

- A script that used modules is longer and starts with the bundle prelude.
- Unpacking such a save writes the bundled text to `script.lua`. The round
  trip of a bundled save is therefore: edit the unbundled tree, pack with
  `--include`, and do not unpack the result into the same tree.
- Line endings follow the normal rules (LF by default; with `--keep-eol` the
  recorded style of the script file, applied to the whole bundle).

### Manual check

Not automated, because it needs the game (the unit tests check the structure,
and the bundle was run once with a stand-alone Lua interpreter):

1. Make a tree with a global script that requires two modules, one of which
   requires a third, and a `ui.xml` with an `<Include>`.
2. `ttskit save pack TREE -o Bundled.json --include lib`.
3. Copy the file into the game's Saves folder, load it and check that the
   script runs (for example with `print` calls in `onLoad`) and that the UI
   shows the included part.

## Asset placeholders

An unpacked tree may name a pushed asset by a placeholder instead of its URL, so
the sources do not change when the asset is uploaded again and its URL changes.

`ttskit save pack DIR -o FILE [--config FILE] [--allow-stale]`

- **Syntax:** `asset:{PATH}`. `PATH` is relative to the project's local
  directory (`[local] dir` of `ttskit.toml`), uses `/` as separator and is
  matched exactly (case included). A placeholder without a closing `}`, with an
  empty path or with a line break inside is left as it is.
- **Where:** in any string value of the JSON files, and in Lua and XML files.
  Placeholders are replaced after bundling, so also inside modules inlined with
  `--include`.
- **Which URL:** the one in the project manifest (`ttskit.manifest.json`, next to
  the config) for the entry whose `path` is `PATH`. The config is `--config`, or
  `ttskit.toml` in the current directory if it exists; without a config (or
  without a manifest file) the map is empty. The config and the local files are
  only read when the tree contains placeholders. If several entries have the same
  path, the one whose SHA1 equals the local file wins, else the first.
- **Errors:** a placeholder without an entry fails the pack, and so does an entry
  that is stale. All offending paths are listed and nothing is written.
- **Stale:** an entry is stale when its local file exists and its SHA1 differs
  from the one recorded at push. A missing local file is not stale (the pushed
  URL is still valid). Push again (`ttskit cloud sync push`) or pass
  `--allow-stale` to use the old URL anyway; pack then prints a warning.
- **Dry run:** `--dry-run` also prints one line per resolved asset, for example
  `asset Boards/a.png -> URL (2 uses)`, with `[stale]` appended where it applies.
  Without `--dry-run`, pack prints `Resolved N asset placeholders (M distinct)`
  when there were any. With `--json` the result has an `assets` array of
  `{path, url, stale, count}`.

`ttskit save unpack SAVE -o DIR --assets [--config FILE]` goes the other way: every
occurrence of the URL of a manifest entry with a local path, in any string, is
written as its placeholder. Only the exact URL text is mapped; the same URL spelled
differently (`http` instead of `https`, another case or a different query) stays
a URL. It is an error when no config or no manifest entry with a local path is
found. Without `--assets`, unpack leaves URLs alone.

`ttskit watch` and `ttskit editor save-and-play` (and the MCP `save_and_play`
tool) pack the tree in memory the same way, so they resolve placeholders too
(`--config FILE`, else `ttskit.toml` in the current directory; the MCP server
uses the one in its project root). A stale asset is not an error there: the
edit loop uses its pushed URL and `watch` prints the warning.

## Reserved for later features

- **Bundling (#9):** implemented, see [Bundling](#bundling). The layout above
  does not change.
- **Asset placeholders (#47):** implemented, see
  [Asset placeholders](#asset-placeholders). Markers and placeholders use
  different prefixes and different fields, so they cannot be confused.

## Prior art: tts-save

The `tts-save` tool (`@tts-tools/savefile` in the `Sebaestschjin/tts-tools`
repository) unpacks saves into a similar tree. Differences:

| | tts-save | ttskit |
|---|---|---|
| Files | `Data.json`, `Script.ttslua`, `UI.xml`, `State.txt` (optional) | `object.json`, `script.lua`, `ui.xml`, `state.json`/`state.txt` |
| Children | `Contents.json`, `States.json`, `Children.json` with paths; child directories next to the files | `contents/`, `states/`, `children/` with `index.json` |
| Directory names | `<Nickname or Name>.<GUID>`, characters outside a fixed set become `-`, collisions get `.1`, `.2` | as specified above, with length limit, reserved names and `-2` suffix |
| Key order | extracted keys are appended at the end on embed | markers keep the original position |
| Empty fields | missing script files become `""` on embed | removed or kept exactly as in the save |
| Round trip | not byte-exact; numbers in arrays and exponents can change | lossless; byte-exact with `--keep-eol` |
| JSON format | LF, no sorting by default | native line endings, structure as the game writes it |
| Line endings of scripts | as in the save | native on disk |

The layouts are not compatible. Importing a tts-save tree is out of scope for
now and may be added later.
