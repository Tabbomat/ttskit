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

## Not yet observed

Only the reply to "get scripts" (ID 1) has been captured from a real game.
Framing of the other incoming IDs (print, error, custom, return, saved, object
created) is assumed to be the same and is covered only by the fake server.
