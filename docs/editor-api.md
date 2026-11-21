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
- The game connects to `127.0.0.1:39998` (IPv4). A listener on `::1`, or an
  IPv6-only one on `::`, receives nothing from it (observed).

### Sharing port 39998 with other programs (observed on Windows)

- Two listeners on `127.0.0.1:39998` are impossible: the second bind fails.
- Binding `127.0.0.1:39998` **succeeds** while another program listens on the
  wildcard address: `0.0.0.0`, or `::` with dual stack, which is what
  Node.js's `server.listen(39998)` does. The game's connections then all go
  to the `127.0.0.1` listener, and the other program silently receives
  nothing. The same happens the other way round: a Node.js program that
  starts listening later gets no error and no messages. Probing by binding
  `0.0.0.0` or `::` does not detect this (both binds succeed).
- An editor integration that listens on `localhost` with a current Node.js
  gets `::1` and so receives nothing from the game, with or without
  `ttskit`.

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
  other editor integrations. Before binding, `Listener::bind` connects to the
  port: if anything accepts within 200 ms, the port counts as in use, which
  catches the wildcard listeners described above. The probe is an empty
  connection, closed at once; the other program sees a connection without
  data. A port that nobody listens on is only reported as refused after
  about 2 s on Windows, so the probe gives up after 200 ms instead, and every
  command that listens starts about 200 ms later.
- A connection that closes without sending anything (such as a probe from
  another `ttskit`) is skipped silently.
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

**After Save & Play (observed):** the game sends "game saved" (ID 6, with
`savePath`) about 10 ms later, then "loaded" (ID 1, with the `scriptStates` of
the reloaded game and `savePath`) once the reload is done (about 0.6 s for a
small save). The output of `onLoad` (prints and errors) comes around it: in a
later check, the print and the error of Global's `onLoad` arrived just
before "loaded", and a print from an object's script just after it. The
reply to "get scripts" also carries `savePath`, the full path of the loaded
save.

**Without being asked (observed):** the game sends "game saved" (ID 6) for
an autosave (`savePath` is the autosave file), and "loaded" (ID 1) when a
save is loaded from the menu, also after the game was restarted. A message
from Lua's `sendExternalMessage(table)` arrives as ID 4. Spawning an object,
or setting its script with `setLuaScript`, sent no message (only the new
script's own `onLoad` print).

**Empty Global script (observed):** Save & Play with a Global script of exactly
`""` makes the game put its default Global script back and stop listening on
port 39999. The reload itself looks normal (chat, System log and `Player.log`
show no error), and the game being focused or in the background makes no
difference. Loading the save again from the menu does not bring the port back;
pressing Save & Play in the game's own Scripting editor does (as does
restarting the game). Not affected: a Global script of `" "`, `"--"` or `"\n"`
(stored as `""`), an empty Global UI, and an empty object script or UI. Since
`ttskit` cannot repair this from outside, it never sends an empty Global
script (see `editor save-and-play`).

Object created (7) was not captured; its framing is assumed to be the same and
is covered only by the fake server.

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
  alone (CRLF vs LF) are not a change, and neither is one trailing newline:
  the game drops exactly one trailing newline from every script and UI it is
  sent (observed: `"x\n"` is stored as `"x"`, `"x\n\t\n"` as `"x\n\t"`), so a
  file that ends with a newline matches the game's copy without it.
- It prints a diff of every change. Without `--yes` nothing is sent;
  `--dry-run` never sends, even with `--yes`.
- A field that has text in the game but none in the source is a `delete`. It
  is refused unless `--allow-delete` is given, and is then sent as an explicit
  empty string.
- Global (`-1`) is never deleted: a Global script or UI missing from the source
  leaves the game's text alone. Observed: an empty Global UI or script makes
  the game put its default UI comment or default script back, so the deletion
  would show up again on every run.
- An empty Global script is never sent; `"\n"` is sent instead, which the game
  stores as an empty script. See "Empty Global script" above for why.
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
  or replay them. Start `console` before the action you want to watch.
  `ttskit mcp` listens for its whole life and buffers what arrives.
- `console` holds port 39998 while it runs, so no other `editor` command (or
  editor integration) can listen at the same time, and the other way round. A
  busy port is reported as for the other commands.
- It never connects to the game, so it does not notice that the game is not
  running.
- The game's own scripts print at any time; nothing ties a line to a request.

### `ttskit watch DIR [--include LIB...] [--once] [--pack-to SAVE.json]`

A fast edit loop on top of `save-and-play`. Not under `editor` because it also
reads the tree and can write the packed save.

```
ttskit save unpack mysave.json -o mymod
ttskit watch mymod --include lib
```

- Watches `DIR` and every `--include` directory recursively (the `notify`
  crate). Nothing is pushed at start; the first push follows the first change.
- A change starts a push only after `--debounce-ms` (default 300) without
  further file events, so a burst of writes from an editor is one push. Each
  write restarts the wait.
- A push packs the tree in memory exactly like `save pack --include`
  (markers, `require` and `<Include/>` bundling), reads the live scripts and
  sends Save & Play for the objects that changed, each with both fields from
  the live state (the same planning as `editor save-and-play`). Nothing
  changed means nothing is sent and the game is not reloaded.
- **No `--yes`**: running `watch` is the consent to reload the game on every
  push, which discards unsaved changes in it. Scripts or UI that the source
  lacks but the game has are deleted, as with `save-and-play --allow-delete`.
- Only scripts and UI are pushed. When another file of the tree changes (for
  example `object.json`, `save.json`, `state.json`) the output says so: that
  needs a full `save pack` and loading the save in the game. Editor temp files
  (`~`, `.swp`, `.tmp`), hidden directories (`.git`, `.ttskit`) and
  directories are ignored; in an include directory only `.lua`, `.ttslua` and
  `.xml` files count.
- A bundling or pack error, an unreachable game or a refused plan is printed
  to standard error and the watch keeps running. A broken file watcher or an
  unusable listener ends it with exit status 1.
- The game's `print` output and Lua errors are shown as in `editor console`,
  including those the reload itself produces (`onLoad`). Messages that arrive
  while a push is waiting for the script list are dropped.
- `--pack-to SAVE.json` also writes the packed save on every change: atomically
  (temporary file next to it, then rename), only when the content changed, and
  an existing file is overwritten. A failed write is a warning; the push still
  happens.
- `--once` packs, pushes once and exits (for editor tasks), then shows console
  output until `--quiet-ms` (default 1000) pass without a message. Exit status
  as above: 0, 1 for a bad tree or a refused plan, 3 if the game is
  unreachable.
- Ctrl+C stops the watch with exit status 0.

Manual check (needs the game; every push reloads it): with a tree that holds
the global script, run `ttskit watch DIR --include LIB --pack-to out.json`,
then (1) write the script several times in quick succession, (2) edit a module
in `LIB`, (3) `touch` `save.json`, (4) require a module that does not exist,
(5) fix it with a script that prints and raises an error in `onLoad`. Observed:
(1) and (2) give one push each and the print from the reload appears; (3)
prints the note and pushes nothing; (4) prints the module error and keeps
watching; (5) pushes, shows the `print` and the Lua error with its object
GUID; `out.json` holds the packed save after each push. An unchanged tree with
`--once` leaves the game alone.

### `ttskit mcp`

A Model Context Protocol server on stdio (the `rmcp` crate) for AI assistants.
Stdout carries only the protocol; it exits 0 when the client closes stdin.
`--root DIR` (default: the working directory), `--console-buffer EVENTS`
(default 1000) and the shared `--game-port`, `--editor-port` and `--timeout`
flags configure it. Setup for clients: see the README.

- **Port use:** the server binds the editor port at start and holds it until
  it exits. If the port is in use (including the wildcard case described in
  "Sharing port 39998 with other programs"), it does not start: exit status
  1 with the busy-port error on stderr. While it runs, `ttskit watch`,
  `editor console` and the other `editor` commands cannot listen and fail
  with the busy-port error; their probe is an empty connection that the
  server skips. Another editor integration that binds `127.0.0.1:39998`
  gets an error too. One that listens on the wildcard address (Node.js's
  default) starts without an error but receives nothing while the server
  runs; stop the server, or that integration, to switch. Requests to the
  game (port 39999) are not affected: other tools can still send.
- **Listener failures:** a connection that cannot be read or decoded is
  counted (`undecodable` in `get_status`) and skipped. Ten accept failures in
  a row count as a broken listener: it is dropped, `listener.state` becomes
  `failed` with the error, tools that need replies fail at once with a "not
  listening" error, and binding the port again is tried every 2 s.
  `get_console` keeps serving the buffer.
- **Game restarts:** the listener does not depend on the game. While the game
  is not running, tools that send fail with `gameUnreachable: true`; when it
  is back, they work again without restarting the server. The game's
  messages about saving and loading show up as `saved` and `loaded` events.
- **Event buffer:** every message the game sends is read, also between tool
  calls. Console output (`print`), Lua errors (`error`), messages from Lua's
  `sendExternalMessage` (`custom`), "game saved" (`saved`, ID 6) and
  "loaded" (`loaded`, ID 1, with `savePath` and `objectsWithScripts` but not
  the scripts) are buffered, each with a `seq` that increases by one per
  event from 1 and the time it arrived. The answer to the server's own "get
  scripts" request is not an event. The buffer keeps at most
  `--console-buffer` events and 4 MiB of text (messages, errors, custom
  messages as JSON, paths) and drops the oldest first; the newest event
  always stays, even if it is larger. Other messages (object pushed, return
  values, unknown IDs) are not buffered.
- **Results:** JSON text, using the same structures as the `--json` output of
  the CLI. Failures the assistant should see (game unreachable, busy port,
  timeout, Lua error, refused plan) are tool results with `isError: true` and
  a JSON body with `error` and `gameUnreachable`. Invalid arguments are
  rejected by the schema before anything is sent.
- `get_status`: a "get scripts" round trip; reports `gameReachable`,
  `savePath`, `objectsWithScripts`, ports, root and version, and the
  server's own state: `listener` (`state` `listening` or `failed`,
  `address`, `since`, and `error` when failed), `lastMessageAt` (the last
  message from the game, the round trip's answer included), `buffer`
  (`events`, `bytes`, `lastSeq`, `evicted`), `undecodable` and
  `lastUndecodable`. It never fails: an unreachable game or a failed
  listener is `gameReachable: false` with `error`.
- `get_scripts`: without arguments, guid, name and the byte sizes of script
  and UI of every listed object, plus `savePath`. With `guids`, the full
  script and UI of those objects and the `missing` GUIDs that the game does not
  list.
- `execute_lua`: as `editor exec --json` (`guid`, `prints`, `errors`, `ok`,
  `returnValue` only if sent), plus `lastSeq`, the newest `seq` when the
  call ended. `guid` defaults to `-1`, `quiet_ms` to 1000. A Lua error makes
  the result an error, with the hints for an unknown GUID or an object
  without a script. The prints and errors are also in the buffer.
- `send_custom_message`: `message` must be a JSON object (checked by the
  schema); returns `{"sent": true}`. The game does not acknowledge it.
- `get_console`: reads the buffer. Events are `editor console --json`
  objects with `seq` added for `print` and `error`, `{seq, time, type:
  "custom", customMessage}`, `{seq, time, type: "saved", savePath}` and
  `{seq, time, type: "loaded", savePath, objectsWithScripts}`.
  - `since`: only events with a greater `seq`. Without it, the newest
    `limit` events; `omitted` counts older matching ones left out. A `since`
    beyond the newest event (for example from before a server restart) is an
    error.
  - `kinds`: a list of `print`, `error`, `custom`, `saved`, `loaded`
    (default: all). An empty list is an error.
  - `limit`: default 100, 1 to 1000. With `since`, the oldest matching events
    come first and `more: true` says that others follow.
  - The result has `events`, `nextSince` (pass it as `since` next time:
    every event up to it was returned or filtered out), `more`, `omitted` and
    `dropped`: events after `since` (or all, without it) that the buffer
    dropped before they were read.
  - It returns at once. With `wait_ms` (at most 60000) and no matching event
    yet, it waits up to that long for one, then keeps collecting until
    `quiet_ms` (default 1000) pass without another match, at most 60 s in
    all.
  - It only reads the buffer, so it does not wait for a running tool call.
- `save_and_play`: `dir` is a tree written by `save unpack`, `include` a list
  of library directories, both relative to the root or absolute, and both
  must resolve inside the root (`..` is refused, symlinks are resolved first).
  The plan is the one `editor save-and-play` makes, with the same rules.
  - Without `confirm` nothing is sent. The result holds `changes`,
    `untouched`, the `diff` as the CLI prints it, `deletes` and a
    `fingerprint`: 16 hex digits of a SHA-256 over every change (GUID,
    listed, and the kind, old and new text of script and UI).
  - With `confirm` set to the fingerprint of the current plan, it is sent. A
    fingerprint that no longer matches (the tree or the game changed) is
    refused with the new plan and fingerprint. A plan that deletes something
    also needs `allow_delete: true`.
  - After sending it waits up to `--timeout` for the game's "loaded" message,
    then collects console output until `quiet_ms` (default 1000) pass without
    a message. The result has `sent: true`, `reloaded`, `console` (the
    `print` and `error` events after the send, with `seq`) and `lastSeq`.
    The `saved` and `loaded` events of the reload are in the buffer. Without
    a "loaded" message in time, `reloaded` is false and a `note` says that
    the output may be incomplete (read the rest with `get_console`).
  - Nothing changed: nothing is sent and the game is not reloaded.

Manual check (needs the game; Save & Play reloads it): drive `ttskit mcp
--root DIR` from a small stdio client (or an assistant), with `DIR`
holding a tree unpacked from the loaded save.

1. `get_status`, then `get_scripts` with and without `guids`.
2. `execute_lua` with a print, with `error()`, with a syntax error, with an
   unknown GUID and in an object without a script.
3. `send_custom_message` after defining `onExternalMessage` with
   `execute_lua`, then `get_console` while `Wait.time` prints.
4. `save_and_play` with no change, with a changed Global script that prints
   and raises an error in `onLoad` (plan, then confirm), with a stale
   fingerprint, with a deleted object script (refused, then `allow_delete`),
   with a first script for an object without one, with an unknown GUID,
   with an `include` bundle, and with paths outside the root.

Observed on Windows with a new single-player game. Every tool behaved as
described above: the `onLoad` print and error came back in the confirm result
together with `reloaded: true`, a source GUID the game does not have was
sent without effect, and paths with `..` or outside the root were refused
before the game was contacted.

Long-running mode, checked on Windows against the game with a small save:

1. `execute_lua` that prints, then prints, sends a custom message and
   raises an error from `Wait.time` callbacks; no call for 3 s; then
   `get_console` with `since` set to the `lastSeq` of `execute_lua`.
   Observed: the later print, the custom message and the error came back
   with the next `seq` values.
2. `ttskit editor exec` from another process while the server runs: refused
   with the busy-port error; the server did not count its probe as
   undecodable.
3. `get_console` with `wait_ms` while a `Wait.time` print is pending:
   returned it about a second later.
4. `save_and_play` with a Global `onLoad` that prints and raises an error:
   `console` had both with their `seq` and `lastSeq`; `get_console` showed
   `saved`, the print, the error, `loaded` and the later prints.
5. A Node.js listener on port 39998 (the default dual-stack `::`) started
   first: the server did not start (busy-port error); Node.js saw one empty
   connection. Started while the server ran: Node.js listened without an
   error and received nothing; the server kept receiving.
6. With the server running: an autosave appeared as `saved`; loading
   another save from the menu as `loaded` with its path; quitting the game
   gave `gameReachable: false` with the listener still `listening`;
   starting it again and loading a save brought `loaded` and the `onLoad`
   output into the buffer, and the tools worked again, without restarting
   the server.

## Flags and exit codes of `ttskit editor` and `ttskit watch`

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

On Windows a refused connection to a local port is only reported after about
2 seconds (observed), because the connection attempt is retried. With
`--timeout 2` or less, an absent game therefore shows up as a timeout (status
1, and `gameUnreachable: false` in `ttskit mcp`) instead of status 3.

## Workshop mods

The game prints "cannot commit save changes to a workshop mod" for Save & Play
on a workshop mod. The scripts still change in the running game, but cannot be
written back to the mod; work from your own save.
