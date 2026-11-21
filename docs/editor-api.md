# External Editor API: wire behaviour

Unofficial notes for the `ttskit-editor` crate; not affiliated with Berserk Games.
The official documentation does not describe framing, so this records what was
observed against a running game (Windows, single-player, a large mod loaded).

## Connections

- The game listens on `127.0.0.1:39999` for messages from the editor.
- The editor must listen on `127.0.0.1:39998`. The game opens a **new connection
  for every message** it sends.
- A reply (for example to "get scripts") is never sent on the request's
  connection. The game accepted the request, sent nothing back on it, and
  opened a fresh connection to port 39998.

## Framing

- One message is one JSON document per TCP connection. There is no length
  prefix and no delimiter. The sender closes the connection after the document,
  so the reader reads until EOF.
- Observed: a 410 KB message arrived as several chunks (about 64 KB each) on a
  single connection and ended with EOF. The game's JSON is pretty-printed with
  CRLF line endings and has no trailing newline.
- The game accepted a request sent without a newline. `ttskit-editor` writes the
  compact JSON and then shuts down the write side; this was verified against the
  real game.
- Field order is not fixed (`messageID` came last in the reply that was seen).

## What `ttskit-editor` does

- `send` opens one connection per message, writes it, and closes. Connection
  failures are reported as "is Tabletop Simulator running?", with a timeout.
- `Listener` accepts one connection per message and reads to EOF, with a
  read timeout and a 64 MiB cap. A bad document is an error for that call only;
  the listener keeps working.
- Binding a port that is already in use gives a dedicated error that mentions
  other editor integrations.
- Unknown fields are ignored; unknown message IDs are returned as
  `Incoming::Unknown` with the full object.

## Manual check

With the game running and a save loaded, and no other editor integration
holding ports 39998/39999:

```
cargo test -p ttskit-editor --test real_game -- --ignored --nocapture
```

## Other incoming messages

Observed with Execute Lua (`print` and a deliberate syntax error): print (ID 2)
and error (ID 3) arrive exactly like ID 1: a new connection, pretty-printed JSON
with CRLF, closed by the game after the document. The error message carries
`error`, `guid` and `errorMessagePrefix`.

## Not yet observed

`return 42` in an Execute Lua script produced no return message (ID 5) in the
test, so its trigger conditions are unknown (it may need a different call shape
or newer game version). Custom (4), game saved (6) and object created (7) were
not captured; their framing is assumed to be the same and is covered only by the
fake server. Slice #23 (execute Lua) should investigate ID 5.

## Commands

### `ttskit editor get-scripts`

Read-only. Asks the game for the scripts of the loaded game and lists the
objects that have a script or UI (the game lists only those; the Global script
has GUID `-1`). `--json` prints the full texts. `--game-port` and
`--editor-port` change the ports.

### `ttskit editor save-and-play DIR`

`DIR` is a tree written by `ttskit save unpack`. It is packed in memory first
(so `--include` bundling and all pack errors behave as in `save pack`) and the
`LuaScript` and `XmlUI` of the global part and of every object at any depth
(`ObjectStates`, `ContainedObjects`, `ChildObjects`, `States`) are matched to
the live game by GUID.

Save & Play deletes the script or UI of every listed object that is missing or
null in the message, and reloads the whole game, discarding unsaved changes in
it. The command guards against that:

- It always fetches the live state first and compares. Only objects whose
  script or UI differ are sent; every sent object carries **both** fields in
  full (an unchanged field is sent exactly as the game has it). Line endings
  alone (CRLF vs LF) are not a change.
- It prints a diff of every change. Without `--yes` nothing is sent;
  `--dry-run` never sends, even with `--yes`.
- A field that has text in the game but none in the source is a `delete`. It
  is refused unless `--allow-delete` is given, and is then sent as an explicit
  empty string.
- Objects only in the game are left alone. Objects of the source that the game
  does not list (the game lists only objects that already have a script or
  UI) are reported and skipped: whether Save & Play accepts a first script for
  such an object is untested.
- A GUID that occurs several times in the source with different scripts is an
  error (duplicate GUIDs are common in real saves).
- Nothing changed: nothing is sent and the game is not reloaded.

Manual check (needs the game; it reloads the loaded game): fetch with
`get-scripts`, unpack a save of that game, edit one script, run
`save-and-play DIR --dry-run`, then with `--yes`, and confirm the change in the
game's script editor.
