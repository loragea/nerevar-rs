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
- [Co-admins](#co-admins)
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
the daemon advertises it in the manifest summary.

**What a host controls is the version, not where it comes from.** A client
ships its own list of trusted runtime repositories (the official
`tes3mp/tes3mp`, plus any a player has added under **Settings → Trusted
runtime sources**), and a hint may only *select among* those. Hint a
repository the player trusts and their picker preselects it with the release
you named; hint anything else and the client says your suggestion was not one
of its trusted sources, preselects the official repository with no release
chosen, and downloads nothing from yours. That is deliberate: a host that
could name the repository could name one whose "TES3MP" is any executable it
likes.

The `tag`, though, is honoured either way. At every sync a connected client
compares it against the build it has installed and, when they differ, offers
to install the required version — from *its* repository for this instance —
and refuses to launch until it has. So a fork server's players need to add
the fork's repository once, in Settings, before the hint does anything;
without that they get the message and no download. Tell them the repository
slug out of band, alongside your address and password.

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
left empty) *and* what it enforces against every connected player's installed
runtime. Only `"kind": "githubRelease"` can be advertised — a path on this
machine means nothing on a player's — and the repository must be public, since
Nerevar does not sign in to GitHub. Omit the key entirely to suggest nothing,
which is the default; omit `tag` and you pin no version. `--check` reports what will be advertised, and says so
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
             [--tls-cert <path> --tls-key <path>]
```

With no subcommand it runs as the daemon, as below. `nerevar-host admin …`
manages co-admin tokens and exits without binding anything — see
[Co-admins](#co-admins).

| Flag | Effect |
| --- | --- |
| `--config <path>` | Config file to read. Default: the per-user GUI config (`$XDG_DATA_HOME/dev.kyleaustad.nerevar/config.json`) if it exists, else `/etc/nerevar/config.json`. Never created automatically. |
| `--instance <id-or-name>` | Which owned instance to host (id first, then name). Optional when the config has exactly one. |
| `--port <n>` | Sync-port override for this run only; the config file is not touched. |
| `--scan` | Rescan `data/` and merge into `load-order.json` before hosting. |
| `--no-manifest-rebuild` | Serve the `manifest.json` already on disk instead of rebuilding it. |
| `--sync-only` | Host sync only; don't launch the TES3MP server. |
| `--check` | Print the summary above and exit 0/1 without starting anything. |
| `--tls-cert <path>` | PEM certificate chain; serves the sync port over HTTPS. Requires `--tls-key`. Also read from `NEREVAR_TLS_CERT`. See [HTTPS](#https). |
| `--tls-key <path>` | PEM private key for that certificate. Requires `--tls-cert`. Also read from `NEREVAR_TLS_KEY`. |

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

## Co-admins

A co-admin is somebody you let change the host's mod list over HTTP without
giving them a shell. They authenticate with a **named bearer token**, separate
from the sync password players use — so `admin list` and the daemon's log say
*who* did something, and revoking one person does not change anyone else's
credentials.

Create one:

```
nerevar-host [--config <path>] [--instance <id-or-name>] admin add ada
```

The token — 64 hex characters, 32 random bytes — is printed to **stdout,
alone**, so `nerevar-host admin add ada > ada.token` captures the secret and
nothing else. Everything else the command says goes to stderr. It is shown
once and cannot be recovered: only its SHA-256 is stored. If it is lost,
revoke the name and add it again.

```
nerevar-host admin list             # name, role, created-at (never hashes)
nerevar-host admin revoke ada       # the token stops working immediately
```

The store is `<data dir>/.nerevar/admins.json`, owned by the daemon and
written `0600`. The GUI never touches it, so a desktop user editing the same
instance will not wipe it. The daemon re-reads it on every admin request, so
`admin add` and `admin revoke` take effect against a running daemon — no
restart, no signal. These commands exit `0` on success and `1` on failure
(unknown instance, duplicate name, unknown role), like every other non-daemon
path.

### Using a token

Send it as a bearer header on the sync port:

```
curl -H "Authorization: Bearer $(cat ada.token)" http://myhost:25567/admin/status
```

`GET /admin/status` reports whether hosting is active, the instance, load-order
counts, when the manifest was generated and how old it is, the pending change
set (`pendingChanges`: staged packages, packages marked for removal, whether a
load order is waiting — `null` when nothing is staged), whether a running
TES3MP server is behind the served manifest (`tes3mpPluginListStale`), and —
when the daemon supervises the game server — whether TES3MP is running
(`tes3mpServerRunning`) and when it was launched (`tes3mpServerStartedAt`,
RFC 3339, `null` when it is not running). A missing or
unrecognised token is `401`; a valid token whose role does not grant the route's
capability is `403`. Both come back as `{"error": "..."}`. Every authenticated
request is logged with the admin's name, the method, the path, and the response
status, at `info`, so `journalctl -u nerevar-host` is the audit trail.

### Changing the mod list over HTTP

Co-admin writes are **staged, then applied**. Everything a co-admin uploads or
marks lands in `<data dir>/.nerevar/staging/` — never in `data/` — and one
`POST /admin/apply` turns the whole set into a new `data/`, a new
`load-order.json` and a new `manifest.json`. So a co-admin can stage over
several days, another admin can review `GET /admin/status` first, and one
approval publishes the lot. `POST /admin/discard` throws the pending set away.

Assume `H="http://myhost:25567"` and `A="Authorization: Bearer $(cat ada.token)"`.

**Upload a package.** The body is the archive itself — `.zip` or `.tar.gz`,
detected from its content — and the URL's last segment is the directory name
the package will take in `data/`:

```
curl -X PUT -H "$A" --upload-file "Better Bodies.zip" "$H/admin/packages/Better%20Bodies"
```

The reply says what landed: `name`, `kind` (`mod` or `replacer`), the `plugins`
found, `archiveBytes`, `extractedBytes`, and `replacesExisting`. If the archive
holds **exactly one entry and it is a directory**, that wrapper is stripped and
its contents become the package; anything else is taken as-is, and only one
level is ever removed. A name that already exists in `data/` means *replace at
apply*, which `status` reports. A name is one plain path segment: no `/`, no
`..`, no `.`-leading or reserved names (`data`, `tes3mp`, `.nerevar`), at most
128 characters. The body limit on this route only is **4 GiB**; if you front the
daemon with a reverse proxy, raise its limit for `PUT /admin/packages/*` too.

**Set the load order.** The body is exactly the `load-order.json` document the
desktop app writes, so the rules are that file's rules; on top of them an entry
may name a staged package, and may not name a package that will exist nowhere
after apply. Fetch, edit, post back:

```
curl -H "$A" "$H/admin/load-order" > order.json     # {"current": ..., "pending": ...}
jq .current order.json | ... > proposed.json
curl -X POST -H "$A" -H 'Content-Type: application/json' \
     --data @proposed.json "$H/admin/load-order"
```

**Remove a package.** A staged upload is dropped outright; a package in `data/`
is *marked*, and apply is what deletes it. `404` if it is neither.

```
curl -X DELETE -H "$A" "$H/admin/packages/Old%20Mod"
```

**Apply, or throw it away.**

```
curl -X POST -H "$A" "$H/admin/apply"      # returns the new manifest summary
curl -X POST -H "$A" "$H/admin/discard"    # clears staging and the pending set
```

Apply is serialized: a second one while the first is running is a `409`.
Applying with nothing pending is a `200` that still rebuilds — that is how you
regenerate a manifest after changing files on the host by hand. Players pick
the new manifest up on their next sync.

**Apply does not restart TES3MP.** The dedicated server reads its plugin list
once, at start, so after an apply the running game server is still enforcing
the old one. The daemon logs a warning and `status` reports
`tes3mpPluginListStale: true`.

**Restart the game server when the players are ready.** This is a separate
call because it **kicks everyone connected** — the game server stops and comes
back, and clients have to reconnect — so an apply publishes the new mod list
and you choose the moment it takes effect:

```
curl -X POST -H "$A" "$H/admin/restart"
```

The reply is `{"restarted": true, "pid": ..., "startedAt": ..., "wasRunning":
...}` with the new process's pid and launch time. The daemon stops the old
process group, waits for the game port to be released, and relaunches with the
`requiredDataFiles.json` the apply wrote; it does **not** treat its own restart
as a crash, so the service stays up and `tes3mpPluginListStale` goes back to
`false`. If the game server dies for any other reason — before, during, or
after a restart — the daemon still exits `69` and leaves the recovery to the
service manager. `systemctl restart nerevar-host` remains the equivalent from a
shell, at the cost of also dropping sync hosting for the few seconds the
manifest rebuild takes. A restart is serialized with apply: a `409` means one
of the two is already running. It is also a `409` on a `--sync-only` daemon,
which has no game server to restart.

### The nerevar-cli command

`nerevar-cli admin` is the same routes with the `curl` removed. It ships from
this repo (`cargo build --release -p nerevar-cli`), needs no Node or Tauri
toolchain, and runs on the co-admin's own machine — the host does not have to
have it. The `--host` address is the one the desktop app's connection form
takes: a hostname or IP plus `--port` (default 25567), or a full `http(s)://`
URL, which then carries its own port.

The token goes in `--token-file <path>`, or in `NEREVAR_ADMIN_TOKEN`, or —
last resort, because the process list is not private — in `--token`. It is
never printed and never appears in an error message.

On the host, once:

```
nerevar-host admin add ada > ada.token     # hand the file to Ada, privately
```

Then, from anywhere Ada is:

```
export NEREVAR_ADMIN_TOKEN=$(cat ada.token)
export H=https://mw.example.org            # or: --host myhost --port 25567

nerevar-cli admin --host $H status
nerevar-cli admin --host $H upload "Better Bodies.zip"
nerevar-cli admin --host $H enable "Better Bodies"
nerevar-cli admin --host $H apply
nerevar-cli admin --host $H restart
```

`upload` streams the archive off disk, so a multi-gigabyte package never lands
in memory at either end, and names the package after the archive's file name
unless `--name` says otherwise. `enable`, `disable` and `order <name>…` are
the fetch-edit-post above done for you: each reads the load order the next
apply will start from — the staged one if there is one, the one on disk
otherwise — changes it, and stages the result. `order` moves the names it is
given to the front, in that sequence, and leaves everything else in its
existing relative order behind them. `load-order get [--pending]` and
`load-order set <file.json>` are there for an edit none of those cover.

Nothing is live until `apply`, and a running TES3MP server keeps the old
plugin list until `restart` — which prompts first, because it kicks everyone
connected; `--yes` skips the prompt for a script.

Every command takes `--json`, which prints the host's response body verbatim
instead of a summary, for scripting. A non-2xx prints the host's own `error`
string and exits 1; a request that never reached a server names the address it
tried.


### Roles

Each admin record carries a **role**, and a role is a named set of
capabilities (`status`, `stage`, `apply`, `restart`, `manage-admins`) declared
in one table in the code. Only `admin` exists today and it grants all of them,
which is why `--role` can be left off. Routes ask for a capability, never for
"is this an admin", so narrower roles are a table entry rather than a rewrite.
A role name in `admins.json` that this build does not know authenticates
nothing — the daemon warns once and treats those records as inert.

### HTTPS

Over a LAN or a VPN, plain HTTP is fine. Over the internet it is not: both the
sync password and a co-admin's bearer token travel in plaintext headers,
readable by anything on the path. Two ways to fix that — the daemon can
terminate TLS itself, or a reverse proxy can do it.

Either way, players and co-admins then enter the `https://` URL as the **host
address** in the desktop app's connection form — the same field that otherwise
takes a hostname or IP. When the address is a URL the sync port field is
ignored: the URL carries its own port, explicitly
(`https://mw.example.org:8443`) or by its scheme.

Only the *sync* port is ever HTTPS. TES3MP is UDP straight to the game port, so
that port stays open on the host and is reached by hostname; Nerevar derives
that hostname from the URL for you.

Clients verify the certificate against the operating system's trust store, and
there is no way to pin or override that today. **A self-signed certificate will
be rejected** — every sync and every admin call fails at the handshake. Use a
publicly trusted certificate from a CA the platform already knows. (Pinning a
host's own certificate is a possible follow-up; it does not exist yet.)

#### Native TLS

Point the daemon at a PEM certificate chain and its private key:

```
nerevar-host --config /etc/nerevar/config.json \
             --tls-cert /etc/letsencrypt/live/mw.example.org/fullchain.pem \
             --tls-key  /etc/letsencrypt/live/mw.example.org/privkey.pem
```

Both flags or neither — one without the other is a startup error (exit 1).
`NEREVAR_TLS_CERT` and `NEREVAR_TLS_KEY` set the same two paths, which is the
tidier form inside a systemd unit. They are command-line/environment settings
rather than config-file keys because `config.json` is the desktop app's file and
is shared with GUI users; certificate paths belong to this host only.

- `--tls-cert` takes the **full chain** in PEM (`-----BEGIN CERTIFICATE-----`,
  leaf first, then intermediates) — certbot's `fullchain.pem`, not `cert.pem`.
  A client that only gets the leaf cannot build a path to the root.
- `--tls-key` takes a PKCS#8 (`-----BEGIN PRIVATE KEY-----`) or RSA
  (`-----BEGIN RSA PRIVATE KEY-----`) key in PEM. Certbot writes PKCS#8.
- With TLS configured, the sync port serves **HTTPS only**. There is no
  cleartext fallback and no second port; a plain `http://` request to it is
  refused at the handshake.

`--check` loads and validates the pair before anything binds, which is why the
example systemd unit runs it as `ExecStartPre`:

```
$ nerevar-host --config /etc/nerevar/config.json --check \
      --tls-cert /etc/letsencrypt/live/mw.example.org/fullchain.pem \
      --tls-key  /etc/letsencrypt/live/mw.example.org/privkey.pem
nerevar-host --check
  config path:    /etc/nerevar/config.json
  instance:       Mundus Patens (mundus)
  …
  sync port:      25567 (https)
  tls cert:       /etc/letsencrypt/live/mw.example.org/fullchain.pem
  tls key:        /etc/letsencrypt/live/mw.example.org/privkey.pem
  tls identity:   CN=mw.example.org (mw.example.org), not after Nov 12 09:41:03 2026 +00:00
  result:         OK
```

A certificate or key that cannot be read, or a key that does not go with the
certificate, fails the check and, on a real run, exits 1 as any other config
failure does. Startup logs one line naming the address and the certificate:

```
nerevar-host[2451]: [INFO  nerevar_core::nerevar_server] NEREVAR SERVER: serving HTTPS on 0.0.0.0:25567 with certificate /etc/letsencrypt/live/mw.example.org/fullchain.pem
```

**Getting a certificate with certbot.** The daemon does not speak ACME; obtain
the certificate separately. Standalone mode needs port 80 free for the
challenge:

```sh
sudo certbot certonly --standalone -d mw.example.org
sudo setfacl -R -m u:nerevar:rX /etc/letsencrypt/live /etc/letsencrypt/archive
sudo systemctl restart nerevar-host
```

The daemon runs as an unprivileged user, so it needs read access to
`/etc/letsencrypt/live/…` and the `archive/` directory the symlinks point into —
the ACL above is one way; a deploy hook that copies the pair somewhere the
service user owns is another.

**Renewal needs a restart.** There is no hot reload: the daemon reads the
certificate once, at startup. Add the restart to the renewal:

```sh
sudo certbot renew --deploy-hook 'systemctl restart nerevar-host'
```

A restart drops connected clients mid-sync; they resume on their next attempt.
If the certificate is within 14 days of expiring, both startup and `--check`
say so — which, with no hot reload, means renewal has silently stopped
happening.

#### Reverse proxy

The alternative: leave the daemon on plain HTTP bound to loopback, and put
nginx or Caddy in front of the sync port. This is the better fit when the host
already sits behind a web server, when TLS should be mounted under a subpath
(`https://example.org/nerevar` works as a host address; a path prefix is
preserved), or when you want certificate renewal to need no service restart.

The proxy has three jobs beyond TLS:

- **Pass `Authorization` and `X-Nerevar-Sync-Password` through unchanged.** Both
  are how the daemon knows who is calling; a proxy that strips or rewrites them
  turns every request into a `401`.
- **Allow large request bodies** on `PUT /admin/packages/*`, which uploads a mod
  package. nginx defaults to 1 MB (`client_max_body_size`); Caddy's default is
  unlimited but `request_body max_size` caps it if you set one.
- **Allow long read timeouts.** A player's first sync streams every file in the
  manifest, which can run for many minutes on a large mod list.

nginx:

```nginx
server {
    listen 443 ssl;
    server_name mw.example.org;

    ssl_certificate     /etc/letsencrypt/live/mw.example.org/fullchain.pem;
    ssl_certificate_key /etc/letsencrypt/live/mw.example.org/privkey.pem;

    location / {
        proxy_pass http://127.0.0.1:25567;
        proxy_set_header Host $host;

        client_max_body_size 0;      # mod package uploads are large
        proxy_read_timeout 3600s;    # first sync streams for a long time
        proxy_request_buffering off;
    }
}
```

Caddy:

```caddy
mw.example.org {
    reverse_proxy 127.0.0.1:25567 {
        transport http {
            read_timeout 3600s
        }
    }
}
```

Caddy needs no body-size directive unless you add one; if you do, give
`request_body max_size` room for your largest package.

Only the *sync* traffic goes through the proxy. TES3MP itself is UDP straight to
the game port, so that port stays open on the host and the game connection is
made to the proxy's hostname, not its URL — Nerevar derives that for you when
you enter a URL address.

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

A co-admin with no shell does the same thing over HTTP instead — upload,
`apply`, then restart when convenient. See
[Changing the mod list over HTTP](#changing-the-mod-list-over-http).

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
| `syncPort` (e.g. 25567) | TCP | Nerevar clients, co-admins | HTTP, or HTTPS when `--tls-cert`/`--tls-key` are set (never both on one port). Manifest + mod-file downloads, password-protected when the server password is set; also the `/admin` routes, which take a bearer token instead. |
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
