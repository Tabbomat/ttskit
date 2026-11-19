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
  only by `--clean` (#8).
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
| Such a file or directory exists, but the key holds a non-marker value or a marker for another file | Error naming both. |
| A directory in `objects/`, `contents/`, `states/` or `children/` is not listed in its `index.json` | Error naming the directory. |
| `index.json` lists a directory that does not exist | Error naming `index.json` and the entry. |
| A prettified `state.json` was edited | It is compacted. If it is no longer valid JSON: error. |
| A JSON file is malformed | Error with the path and position. |
| Unknown files (no marker, not in an index) | Ignored with a warning. |

Errors abort pack before anything is written.

Unpack never deletes files it did not create. Writing into an existing tree and
removing stale files are covered by #8: `--clean` removes only files listed in
`files` of the previous metadata.

## Reserved for later features

- **Bundling (#9):** unpacked scripts and UI keep the unbundled form with
  `require()` and `<Include>`. Pack may inline modules from include
  directories; the layout above does not change.
- **Asset placeholders (#47):** an `asset:<path>` placeholder may appear in any
  string, in `object.json` and `save.json` as well as inside Lua and XML files.
  Markers and placeholders use different prefixes and different fields, so they
  cannot be confused.

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
