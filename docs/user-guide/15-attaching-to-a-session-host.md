# Chapter 15: Attaching to a Session Host

A *session host* is a headless Bilterm that owns the shells and keeps them alive. `bilterm attach`
connects a terminal to it, shows the host's panes and forwards your keys. Close the terminal and the
shells keep running; attach again later. Unix only (Linux, macOS); Windows is not supported yet.

## Start a host

```bash
bilterm --headless --host-ptys --attach --name work
```

`--attach` is off by default and requires `--host-ptys`. The host prints
`bilterm: attach listening on unix:<runtime dir>/host-work.attach.sock`. The host never starts
automatically; nothing spawns it for you.

## Attach

```bash
bilterm attach work              # by host name
bilterm attach /path/to/host-work.attach.sock
bilterm attach work --ro         # read-only: you can watch and switch panes, not type
```

`bilterm attach <name>` means "attach to a host" only if that host's attach socket exists;
otherwise it keeps its older meaning (restore a saved layout session). A path or `*.sock` argument
always means a host socket. `bilterm attach-host <name>` is the unambiguous spelling.

## Keys

The prefix is `Ctrl-]`.

| Keys | Action |
|---|---|
| `Ctrl-]` then `d` (or `q`) | Detach (exit 0). The host and its shells keep running |
| `Ctrl-]` then `n` / `p` | Next / previous pane |
| `Ctrl-]` then `Ctrl-]` | Send a literal `Ctrl-]` to the pane |
| anything else | Sent to the selected pane (not in `--ro`) |

## Exit codes

| Code | Meaning |
|---|---|
| 0 | Detached normally |
| 1 | Cannot connect, not a terminal, or the connection was lost after bounded reconnects |
| 2 | Usage error (bad arguments, unknown host name) |
| 3 | Handshake refused (version mismatch, host full, ...) |
| 4 | Host closed you: kicked, too slow to keep up, or idle timeout |
| 5 | Protocol error |
| 6 | The host shut down |

## Who controls the size

With several `rw` clients, the most recently active one decides the pane size (150 ms debounce).
`--ro` clients never resize. Focus and the active pane are shared by all clients.

## Limits

Caps live in one place, `src/attach/limits.rs`:

| Limit | Value |
|---|---|
| Clients per host | 16 |
| Terminal size accepted | at most 500 x 200 cells |
| Host -> client frame | 4 MiB; client -> host frame 64 KiB |
| One input message | 8 KiB |
| Input rate | 1 MiB/s, 256 KiB burst |
| Handshake | 5 s, 4 KiB before it completes |
| Slow client | resynced after 5 s without acks, closed after 30 s |
| Liveness | ping every 15 s, closed after 3 missed |
| Colon commands from a client | 100 per second, 4096 bytes each |

## Security model

- The attach socket is `host-<name>.attach.sock` in the private runtime directory (directory
  `0700`, socket `0600`), separate from the JSON-RPC/`ctl` socket.
- On accept, the host checks the peer's uid with `SO_PEERCRED`/`LOCAL_PEERCRED` and drops any
  connection from a different user.
- The client refuses a socket that is a symlink, is not owned by you, or is group/other accessible.
- Frames are bounded before anything is allocated; sizes from the peer are validated first.
- There is no token or resume: every connection is a new client. Anyone with your uid can attach
  as `rw`; use `--ro` for watching.

## Inspecting clients

- Colon command: `:daemon attach`
- RPC: `bilterm ctl attach.clients` (MCP tool `attach_clients`); `workspace.status` also reports it.

All three are read-only and report `disabled` when the host runs without `--attach`.

## Kicking a client

- Colon command: `:daemon attach kick <client-id>` or `:daemon attach kick --ro` (every read-only client)
- RPC: `bilterm ctl attach.kick '{"client_id":3}'` or `'{"ro":true}'`
- MCP: `bilterm_daemon` with `action="attach_kick"` and `client_id` or `ro=true`

The kicked client receives `Bye{kicked}` and exits with code 4 ("kicked by the host"); its entry
disappears from `attach.clients` as the connection closes. Other clients are unaffected. An unknown
id is an error (`unknown attach client N`); `--ro` with no read-only clients kicks none and succeeds.
Kicking needs the same access as other daemon-changing commands (the local control socket, same
uid, or the TCP token). An attach client can never kick another: an `Act` containing
`:daemon attach kick` is refused with `bad_request`. A kicked client may reconnect (there is no ban
list); the host logs every kick at `warn` level (`attach.kick`).

## Known gaps

- No Windows client or host attach (named pipes deferred).
- A kicked client can reconnect immediately (no ban list); a host that is over its cap refuses new clients.
- No graphics (Kitty/Sixel) or clipboard proxying through attach yet; no GUI attach client.
- A `bilterm attach` client does not start a host; start it yourself.
- Not yet verified on macOS (the peer-uid call is the same code path as the JSON-RPC socket).
- Smoke test: `scripts/attach_smoke.py` drives a real host and client over a PTY.
