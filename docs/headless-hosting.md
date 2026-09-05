# Headless hosting with `nerevar-host`

The desktop app is one way to host a Nerevar instance; `nerevar-host` is the
other. It is a small foreground daemon that hosts **one owned instance** on a
machine with no desktop: it rebuilds the instance's manifest, serves it (and the
mod files) to Nerevar clients over HTTP, and runs the instance's TES3MP
dedicated server. Everything it does, it does through the same `nerevar-core`
code paths the GUI uses — same config file, same manifest format, same load
order — so a host can be managed from the app on a desktop and then handed to a
server, or run headless from the start.

It is Linux-first (that is where dedicated servers live) and expects a service
manager to own it: no daemonizing, no PID files, no log files. It logs to
stderr, exits on SIGTERM, and lets systemd do the rest.

- [Install the binary](#install-the-binary)
- [Set up an instance without the GUI](#set-up-an-instance-without-the-gui)
- [Run it](#run-it)
- [Run it under systemd](#run-it-under-systemd)
- [Day-2 operations](#day-2-operations)
- [Ports](#ports)
- [Gotchas](#gotchas)

## Install the binary

From a checkout (Rust stable; no Tauri or Node toolchain needed — the daemon
does not depend on the GUI crate):

```sh
cd src-tauri
cargo build --release -p nerevar-host
sudo install -m 0755 target/release/nerevar-host /usr/local/bin/
```

`nerevar-host --version` should answer. The binary is self-contained apart from
the usual system libraries.

## Set up an instance without the GUI

An instance is a directory tree plus an entry in a config file. Nothing in
either is GUI-specific, so both can be created by hand.

### 1. A service user and a home for the instances

```sh
sudo useradd --system --home-dir /srv/nerevar --create-home nerevar
sudo -u nerevar mkdir -p /srv/nerevar/mundus/tes3mp /srv/nerevar/mundus/data
```

The layout the daemon expects, using `/srv/nerevar/mundus` as the instance root:

```
/srv/nerevar/mundus/
├── tes3mp/            # the TES3MP install (tes3mp-server, its cfg, server/)
└── data/              # one directory per mod package
    ├── Better Bodies/
    ├── Tamriel Rebuilt/
    └── .nerevar/      # Nerevar's own state: load-order.json, manifest.json
```

### 2. A TES3MP server install

Unpack a TES3MP release into `tes3mp/`. Any layout works as long as
`tes3mp-server`, `tes3mp-server-default.cfg` and `server/data/` are somewhere
below it — the daemon searches a few levels deep, so
`tes3mp/TES3MP-0.8.1/tes3mp-server` is fine.

Then edit `tes3mp-server-default.cfg`. This file — not the Nerevar config — is
the source of truth for the game port, the server password, and the player cap:

```ini
[General]
port = 25565                  # UDP game port clients connect to
maximumPlayers = 8
hostname = Mundus Patens
password = choose-something    # doubles as the sync password (see below)

[MasterServer]
enabled = true                 # false keeps the server off the public browser
```

The `[General] password` value is what Nerevar clients must send to sync
(`X-Nerevar-Sync-Password`), and it is what the game client asks for on connect.
An empty password means anyone who can reach the sync port can download the mod
list.

### 3. Mod packages

One directory per package under `data/`, each shaped like a Morrowind data
directory (loose `meshes/`, `textures/`, `.esp`/`.esm` files at the package
root, and so on) — exactly what the app's data manager builds. Copy them in
however you like:

```sh
sudo -u nerevar rsync -a --delete "Better Bodies/" /srv/nerevar/mundus/data/"Better Bodies"/
```

Do not put base-game data files in a package — see the mod-redistribution notice
in the [README](../README.md#important-disclaimer-notice--read-before-hosting-or-sharing-mods).
Base game data lives outside the instance and is named once, in the load order's
`baseGameData` field.

### 4. The Nerevar config file

`/etc/nerevar/config.json`, the service-account default the daemon looks for
when `--config` is not given (a desktop user's per-user GUI config is checked
first — see [Run it](#run-it)). The daemon **never creates it**: unlike the app,
there is no onboarding to walk you through.

```json
{
  "onboardingComplete": true,
  "ownedInstances": [
    {
      "id": "mundus",
      "name": "Mundus Patens",
      "description": "Friend-group server",
      "path": "/srv/nerevar/mundus",
      "dataDir": "/srv/nerevar/mundus/data"
    }
  ],
  "syncedInstances": null,
  "rootPath": "/srv/nerevar",
  "syncPort": 25567
}
```

Every key shown is required (`syncedInstances`/`rootPath` may be `null`).
`syncPort` is the TCP port the manifest/file server listens on — Nerevar's own
port, distinct from the TES3MP game port. `id` is what `--instance` matches
(name works too).

#### Suggesting a TES3MP runtime to players (optional)

Players pull their own TES3MP build; the host never sends one. If your server
needs a particular build — a fork that publishes its runtime as GitHub
releases in its own repository, say — add a `runtimeHint` to the instance and
the daemon advertises it in the manifest summary. A connecting player's
runtime picker preselects that repository and release and says the suggestion
came from you; they can still choose anything else.

```json
{
  "id": "mundus",
  "name": "Mundus Patens",
  "description": "Friend-group server",
  "path": "/srv/nerevar/mundus",
  "dataDir": "/srv/nerevar/mundus/data",
  "runtimeHint": {
    "kind": "githubRelease",
    "repo": "owner/name",
    "releaseId": "",
    "tag": "0.8.1"
  }
}
```

`repo` is `owner/name`; `tag` is the release tag as published there, and is
what the client matches against the repository's releases (`releaseId` may be
left empty). Only `"kind": "githubRelease"` can be advertised — a path on this
machine means nothing on a player's — and the repository must be public, since
Nerevar does not sign in to GitHub. Omit the key entirely to suggest nothing,
which is the default. `--check` reports what will be advertised, and says so
when a hint is unusable and will be ignored:

```
  runtime hint:   suggests owner/name 0.8.1 to players
```

### 5. A load order

The load order (`data/.nerevar/load-order.json`) decides which packages are
served, in what order, with which plugins enabled. Build it from what is on disk:

```sh
sudo -u nerevar nerevar-host --config /etc/nerevar/config.json --scan --check
```

`--scan` scans `data/` and merges the result into `load-order.json` — new
package directories are appended (enabled, lowest priority), directories that
have disappeared are dropped, plugin lists are refreshed. It is the headless
equivalent of the data manager's rescan in the app. A scan reads directory
entries and filenames only; it hashes nothing, so it stays fast on a large
`data/`. Checksums are computed by the manifest rebuild that every normal run
does at startup. `--check` then prints what would be hosted and exits without
starting anything:

```
nerevar-host --check
  config path:    /etc/nerevar/config.json
  instance:       Mundus Patens (mundus)
  instance root:  /srv/nerevar/mundus
  data dir:       /srv/nerevar/mundus/data
  sync port:      25567
  tes3mp server:  found (/srv/nerevar/mundus/tes3mp/tes3mp-server)
  load order:     4 package(s), 4 enabled, 3 plugin(s)
  manifest:       none yet (…/data/.nerevar/manifest.json) — a run rebuilds it
  result:         OK
```

To reorder packages, enable/disable one, or turn an individual plugin off, edit
`load-order.json` (`priority` ascending = load order; `enabled` on the entry and
on each plugin). Base-game data goes in `baseGameData`:

```json
{
  "version": 1,
  "baseGameData": "/srv/nerevar/morrowind-data",
  "entries": [ … ]
}
```

## Run it

```
nerevar-host [--config <path>] [--instance <id-or-name>] [--port <n>]
             [--scan] [--no-manifest-rebuild] [--sync-only] [--check]
```

| Flag | Effect |
| --- | --- |
| `--config <path>` | Config file to read. Default: the per-user GUI config (`$XDG_DATA_HOME/dev.kyleaustad.nerevar/config.json`) if it exists, else `/etc/nerevar/config.json`. Never created automatically. |
| `--instance <id-or-name>` | Which owned instance to host (id first, then name). Optional when the config has exactly one. |
| `--port <n>` | Sync-port override for this run only; the config file is not touched. |
| `--scan` | Rescan `data/` and merge into `load-order.json` before hosting. |
| `--no-manifest-rebuild` | Serve the `manifest.json` already on disk instead of rebuilding it. |
| `--sync-only` | Host sync only; don't launch the TES3MP server. |
| `--check` | Print the summary above and exit 0/1 without starting anything. |

`RUST_LOG` sets the log level (`info` by default; `RUST_LOG=debug` adds sync and
scan progress events).

A normal run does this, in order:

1. Load the config, pick the instance.
2. Rebuild `manifest.json` from the load order — hashing every file in every
   enabled package — and rewrite TES3MP's `requiredDataFiles.json` so the
   server enforces exactly the plugins the manifest ships. This is what the
   app's *Save & host* button does, and why clients never see a manifest that
   disagrees with the host's disk.
3. Start the sync server on the sync port and activate hosting.
4. Launch the TES3MP dedicated server, piping its output into the log.
5. Wait. On SIGTERM/SIGINT: stop the TES3MP process group, wait for the game
   port to be released, deactivate hosting, exit 0.

Rebuilding costs a checksum pass over the whole data directory at every start
(seconds for a small list, longer for a big one). `--no-manifest-rebuild` skips
it — a fast restart, at the price of possibly serving a stale mod list, which
the daemon warns about when the load order is newer than the manifest.

Exit codes: `0` asked to stop, `1` startup or config failure, `69` the TES3MP
server exited on its own (the daemon then shuts hosting down with it rather than
stay up looking healthy while nobody can play).

## Run it under systemd

[`packaging/systemd/nerevar-host.service`](../packaging/systemd/nerevar-host.service)
is a complete example unit: `ExecStartPre` runs `--check` so a broken config
fails the unit before any port is bound, `Restart=on-failure` covers both a
crash and the exit-69 case above, and the filesystem-hardening block assumes the
instances live under `/srv/nerevar`.

```sh
sudo install -m 0644 packaging/systemd/nerevar-host.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now nerevar-host
journalctl -u nerevar-host -f
```

The daemon's own lines and the TES3MP server's output both land in the journal;
server output is tagged with the `tes3mp` log target:

```
nerevar-host[2451]: [INFO  nerevar_host::manifest] Built manifest: 4 package(s), 664 file(s), 70.3 MiB, 6 required plugin(s)
nerevar-host[2451]: [INFO  nerevar_host] Sync hosting active for instance "Mundus Patens" (mundus) on port 25567
nerevar-host[2451]: [INFO  tes3mp] [INFO]: TES3MP dedicated server 0.8.1 (Linux 64-bit)
```

To try it without root, the same unit works as a user unit — drop it in
`~/.config/systemd/user/`, remove `User=`/`Group=`/`ProtectHome=` and point the
paths at a directory you own, then `systemctl --user daemon-reload && systemctl
--user start nerevar-host`.

## Day-2 operations

**Add or update mods.** Copy the package directories in, rescan, restart:

```sh
sudo -u nerevar rsync -a "New Mod/" /srv/nerevar/mundus/data/"New Mod"/
sudo -u nerevar nerevar-host --config /etc/nerevar/config.json --scan --check
sudo systemctl restart nerevar-host
```

The restart rebuilds the manifest, so connected clients pick the changes up on
their next sync. There is no hot reload — a restart is the way to apply
anything, including config changes.

**Remove a mod.** Delete the directory and rescan; `--scan` drops entries whose
directory is gone. Clients prune the files on their next sync.

**Disable something without deleting it.** Set `"enabled": false` on the entry
(or on a single plugin) in `load-order.json` and restart. Disabled packages are
not hashed, not served, and not required of clients.

**Change the game port, password, or player cap.** Edit
`tes3mp-server-default.cfg` and restart. The new values propagate to clients
through the manifest.

**Change the sync port.** Edit `syncPort` in the config and restart, or pass
`--port` for a one-off (e.g. to test a second instance without touching the
config).

**Check what is being served** without disturbing the running service:

```sh
curl -H "X-Nerevar-Sync-Password: <server password>" http://localhost:25567/
curl http://localhost:25567/health      # no password needed; "ok" if hosting is up
```

## Ports

| Port | Protocol | Who connects | Notes |
| --- | --- | --- | --- |
| `syncPort` (e.g. 25567) | TCP | Nerevar clients | Manifest + mod-file downloads. Password-protected when the server password is set. |
| `[General] port` (e.g. 25565) | UDP | TES3MP game clients | The game itself. |

Both need to be reachable from the internet (or your VPN) for friends to sync
and play; `/health` on the sync port is a cheap external liveness check.

## Gotchas

- **One TES3MP install per instance.** The daemon writes *inside* the install —
  `requiredDataFiles.json` on every launch, `server/config.lua` game settings on
  every manifest build. Two instances sharing one install (or sharing files
  through hardlinks/symlinks) will fight over those files.
- **The daemon never creates or edits the config file.** If it cannot find one
  it exits with the list of paths it tried. `--port` is runtime-only by design.
- **A missing load order is an error, not an empty mod list.** The daemon
  refuses to guess; run `--scan` once.
- **Manifest rebuild is a full checksum pass.** On a large mod list that is real
  work at every restart. Prefer restarting when nobody is playing, or use
  `--no-manifest-rebuild` when you know nothing changed.
- **The GUI and the daemon share the config file.** Running both against the
  same instance at once means two processes trying to bind the same ports and
  hash the same tree — pick one.
