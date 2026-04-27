# Dev Notes - mam-audiofinder

## Backend

- The app is now implemented in Rust under `src/main.rs`.
- It preserves the same HTTP routes as the Python version:
  - `/health`
  - `/`
  - `/setup`
  - `/api/setup`
  - `/search`
  - `/add`
  - `/history`
  - `/history/{row_id}`
  - `/transmission/torrents`
  - `/import`
- SQLite state lives at `/data/history.db`.
- Runtime config is loaded from `/data/config.json` by default, or `APP_CONFIG_PATH`.

## Container

- The Dockerfile is a multi-stage Rust build.
- The runtime container copies the compiled binary plus `app/static` and `app/templates`.
- The app still expects fixed in-container storage paths:
  - `/downloads`
  - `/library`
  - `/ebooks`

## Local Development

- With a Rust toolchain available, run:
  - `cargo run`
- The app binds `0.0.0.0:8080`.

