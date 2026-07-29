# AGENTS.md — nerevar-rs

Guidance for coding agents (and new contributors) working in this repo.
Nerevar is a desktop companion for TES3MP (Morrowind multiplayer) that keeps
a host and its players synchronized on mods, load order, and game data. See
`README.md` for the full feature overview and the mod-redistribution
disclaimer — never weaken or remove that disclaimer.

## Architecture

Tauri 2 application, three parts:

- `src/` — React + TypeScript frontend (Vite, Tailwind, pnpm). Thin UI layer;
  it mostly invokes Tauri commands and renders state.
- `src-tauri/` — a Cargo workspace with three crates:
  - `crates/nerevar-core` (`nerevar-core`) — the Tauri-free engine: instance
    management (`instance_data/`, `instance_setup/`, `instance_settings/`),
    host/client sync (`sync_host/`, `sync_client/`, `sync_paths.rs`,
    `sync_auth.rs`), the embedded HTTP server (`nerevar_server/`), process
    management (`process_manager/`), config (`config/`), GitHub release
    downloads (`github_getters.rs`), and the supervisor that wires them
    together (`supervisor.rs`, `app_state.rs`). No Tauri dependency, so it's
    reusable by non-GUI frontends — which is what `nerevar-host` is.
    Its `test-util` feature gates test-only helpers (e.g.
    `reporter::CollectingEventSink`) for use from integration tests in
    `crates/nerevar-core/tests/`.
  - `nerevar` (package root, `src-tauri/`) — the Tauri shell: command
    handlers, app/window wiring, GUI-only glue that must stay off of core
    (`file_actions.rs` for native file dialogs, `mo2_plugin.rs`,
    `app_update.rs` for self-update, `TauriEventSink`).
  - `crates/nerevar-host` (`nerevar-host`) — headless daemon: hosts one owned
    instance (manifest rebuild, sync server, TES3MP dedicated server) with no
    GUI, for dedicated Linux servers. Depends only on `nerevar-core` plus
    clap/env_logger/tokio; it must never gain a Tauri dependency, and core
    must never gain a clap one. Operator-facing docs (instance layout without
    the GUI, systemd, day-2 mod updates): `docs/headless-hosting.md` and
    `packaging/systemd/nerevar-host.service`.

Command handlers registered in `src-tauri/src/lib.rs` are the frontend/backend
boundary; most just resolve Tauri state and call straight into `nerevar-core`.
Shared types are exported to TypeScript with ts-rs into `src/types/` at the
repo root (see below) — not `src-tauri/bindings/`, which no longer exists.

## Build and check

- `pnpm install` — once, after clone.
- `pnpm tauri dev` — run the full desktop app.
- `pnpm tauri build` — release build.
- `pnpm dev` / `pnpm build` — frontend only.
- `pnpm typecheck` — TypeScript check; `pnpm format` — Prettier.
- `cargo check --workspace` / `cargo test --workspace` inside `src-tauri/` for
  backend-only work. **Pass `--workspace`**: `src-tauri/` is both the workspace
  root and the `nerevar` package, so a bare `cargo test` runs only that
  package's five tests — it silently skips `nerevar-core`'s ~100 tests, its
  integration tests under `crates/nerevar-core/tests/`, and all of
  `nerevar-host` (which nothing depends on, so a bare `cargo check` never even
  compiles it).
- `cargo build --release -p nerevar-host` — just the headless daemon; needs no
  Node/Tauri toolchain, which is the point on a server.

Rust stable toolchain; frontend uses pnpm (not npm/yarn).

## Ground rules

- Keep business logic in `nerevar-core`; the `nerevar` (Tauri) crate stays a
  thin command/wiring layer, and the frontend stays a thin caller of Tauri
  commands.
- Bindings generate into `src/types/` (repo root), not `src-tauri/bindings/`
  — don't hand-edit generated `.ts` files there, and don't recreate
  `src-tauri/bindings/`. The output path is set by `TS_RS_EXPORT_DIR` in
  `src-tauri/.cargo/config.toml`, which resolves relative to that config
  file. Hazard: that config only applies when cargo runs with `src-tauri/`
  as its working directory (e.g. `cargo test` from inside `src-tauri/`); a
  `--manifest-path` invocation from the repo root skips it and would write
  bindings to the wrong place.
- Watch for platform assumptions (executable names, `.exe` suffixes, process
  flags): the app targets Windows, Linux, and macOS.
- Run `pnpm typecheck` and `cargo check` before considering a change done.
