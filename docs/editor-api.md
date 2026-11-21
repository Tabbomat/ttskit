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

## Execute Lua and custom messages (observed)

Checked against a running game, both with a large mod and with a new empty
single-player game (only the table, default Global script):

- **No return message.** `return 42`, strings, tables, `nil`, `false`, `true`,
  several values, and `return self.getGUID()` in an object all produced no
  return message (ID 5), in Global and in an object with a script. The ID 5
  decoding stays (it is covered by the fake server) but nothing depends on it.
  Use `print(...)` to get output.
- **`print` (ID 2)** from the executed code arrives as usual. The game's own
  scripts may print at any time, and nothing ties a print to a request.
- **Errors (ID 3)** arrive for a runtime error and for a syntax error, with the
  game's text in `errorMessagePrefix` (for example
  `[Global] Lua Error <executeScript>: ...`).
- **Unknown GUID:** `error` holds the GUID and the prefix is
  `Error finding guid for execute script: `. The `guid` field was `-1`.
- **Object without a script:** an existing object that has no script gives
  `Object reference not set to an instance of an object` (with the object's
  GUID in the prefix), so code can only run in an object that already has a
  script. The Global script always exists.
- **Functions persist:** a function defined by an executed script in Global
  stays defined, so `function onExternalMessage(t) ... end` can be defined with
  `exec` and then driven with `send`.
- **Custom message (ID 2 outgoing):** delivered to `onExternalMessage` only
  when it is a JSON object. A number or string is ignored without any error or
  reply. The game does not acknowledge a delivered message.

Game saved (6) and object created (7) were not captured; their framing is
assumed to be the same and is covered only by the fake server.

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
- Objects only in the game are left alone. The game lists only objects that
  have a script or UI. A source object that has one but is not listed gets its
  first script added (verified); it is shown as "not listed by the game". The
  game silently ignores a GUID that does not exist, so a source GUID that is
  not in the loaded game is sent and has no effect, without an error. Source
  objects with no script and no UI are never sent.
- A GUID that occurs several times in the source with different scripts is an
  error (duplicate GUIDs are common in real saves).
- Nothing changed: nothing is sent and the game is not reloaded.

Manual check (needs the game; it reloads the loaded game): fetch with
`get-scripts`, unpack a save of that game, edit one script, run
`save-and-play DIR --dry-run`, then with `--yes`, and confirm the change in the
game's script editor.

Verified once against a real game (Windows, 18 objects with scripts): a tree
built from the live state gave "nothing to do"; appending a comment to one
object's script changed only that object (594 to 623 bytes), and restoring it
brought the game back to the original text. Save & Play reloaded the game
within a few seconds.

Second check on a non-workshop save (68 objects): changing and restoring a
script worked; adding a script to an object listed only for its UI kept the UI
intact; removing it needed `--allow-delete`; a first script on an object that
had neither script nor UI was accepted, and an unknown GUID was ignored.

### `ttskit editor exec [--guid GUID] 'lua code'`

Runs Lua in the context of an object (default `-1`, Global) and prints what the
game reports. Because the game linked no answer to a request and did not send
return values in any check, the command collects instead of waiting for one:

- It sends the code, then reads messages until a return value or a Lua error
  arrives, or until nothing arrives for `--quiet-ms` milliseconds (default 1000),
  or `--timeout` seconds in total (default 5; also bounds connecting).
- `print` output is shown in arrival order, then the return value if the game
  sent one (strings as is, other values as JSON). Prints from the game's own
  scripts during that time are included; there is no way to tell them apart.
- No reply at all is success with no output. The game is not running: error.
- A Lua error is printed to stderr and the command exits with status 1, after
  any output. The error for a GUID that does not exist, or an object without a
  script, carries a hint (see above).
- `--json` prints `{guid, prints, errors, ok}` and, only when the game sent
  one, `returnValue` (so no value differs from a returned `null`). Errors
  still give status 1.

### `ttskit editor send JSON`

Sends a custom message to the game's `onExternalMessage` handler. The message
must be a JSON object; anything else is refused locally, since the game would
drop it silently. The game does not acknowledge the message, so success means
only that it was sent. `--json` prints `{"sent":true}`.

Manual check (needs the game): `ttskit editor exec "print(1+1)"`,
`ttskit editor exec "error('x')"` (status 1), `ttskit editor exec --guid
<object without script> "print(1)"` (status 1 with a hint), then
`ttskit editor exec "function onExternalMessage(t) print(t.hello) end"`, then
`ttskit editor send '{"hello":"hi"}'` and confirm that `hi` is printed in the
game.

### `ttskit editor console [--follow] [--json]`

Prints the game's console output as it arrives: `print` text (this includes
`printToAll`, `broadcastToAll` and `log`, which the game sends the same way)
and Lua errors. Other messages are ignored.

```
20:26:59.858 print  live one
20:26:59.881 error  [-1] [Global] Lua Error <executeScript>: ...
```

- **Time:** the game sends no timestamp. The time shown is when the message
  reached `ttskit` (local time in text, UTC RFC 3339 in `--json`).
- **Origin:** only errors say where they came from: the GUID of the object
  (`-1` is Global) and the game's `[Name]` prefix. A `print` carries nothing
  but the text (checked on a real game for `print`, `printToAll`,
  `broadcastToAll` and `log`), so no object can be shown for it.
- **One-shot (default):** listens until `--quiet-ms` (default 1000) pass
  without a message, or `--timeout` seconds in total (default 5), then exits 0,
  also when nothing arrived.
- **`--follow`:** runs until Ctrl+C and exits 0. It cannot be combined with
  `--quiet-ms`. Each line is flushed at once, so the output can be piped.
- **`--json`:** one JSON object per line, `{time, type: "print", message}` or
  `{time, type: "error", guid, error, message}`.
- A Lua error is output, not a failure: `console` exits 0. A message that
  cannot be decoded is skipped with a warning on stderr.

Limitations:

- Messages sent while no listener is running are lost; the game does not queue
  or replay them. Start `console` before the action you want to watch. A
  long-running listener with a buffer is the subject of the MCP issues.
- `console` holds port 39998 while it runs, so no other `editor` command (or
  editor integration) can listen at the same time, and the other way round. A
  busy port is reported as for the other commands.
- It never connects to the game, so it does not notice that the game is not
  running.
- The game's own scripts print at any time; nothing ties a line to a request.

## Flags and exit codes of `ttskit editor`

Every subcommand takes `--game-port`, `--editor-port` and `--timeout SECONDS`
(connecting, sending and reading one message) and `--json`. `exec` and
`console` share `--quiet-ms` (stop after that long without a message; `exec`
used to call it `--wait`). `--json` prints one object, except `console`, which
prints one object per line so that it can stream.

| Status | Meaning                                                                         |
| ------ | ------------------------------------------------------------------------------- |
| 0      | Success                                                                         |
| 1      | Failure: a Lua error in `exec`, a refused plan, bad input, a busy port, a timeout |
| 2      | Command line usage error (from the argument parser)                             |
| 3      | The game did not accept a connection (not running, or not listening)            |

## Workshop mods

The game prints "cannot commit save changes to a workshop mod" for Save & Play
on a workshop mod. The scripts still change in the running game, but cannot be
written back to the mod; work from your own save.
