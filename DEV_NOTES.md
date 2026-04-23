# Dev Notes – mam-audiofinder

## Recent Key Changes Implemented

- Added a `Settings` helper in `app/main.py`:
  - Loads config from `/data/config.json` (or `APP_CONFIG_PATH`) and falls back to env vars.
  - Centralizes: `MAM_COOKIE`, `TRANSMISSION_URL`, `TRANSMISSION_USER`, `TRANSMISSION_PASS`, `DL_DIR`, `LIB_DIR`, `TRANSMISSION_INNER_DL_PREFIX`, `TRANSMISSION_PATH_MAP`, etc.
- Introduced explicit Transmission → app path mapping:
  - `settings.TRANSMISSION_PATH_MAP` is a list of `(transmission_prefix, app_prefix)` pairs.
  - Populated from:
    - JSON config key `TRANSMISSION_PATH_MAP` (list of objects with `transmission_prefix` / `app_prefix`), or
    - Env `TRANSMISSION_PATH_MAP="/downloads=/media/torrents"`, or
    - Fallback: `TRANSMISSION_INNER_DL_PREFIX` → `DL_DIR`.
  - `do_import` uses this mapping in `map_transmission_path`.
- Added Transmission RPC integration:
  - `POST /add` calls `torrent-add` using either a MAM direct URL or base64 `.torrent` upload.
  - `GET /transmission/torrents` calls `torrent-get`, returning completed torrents with `TRANSMISSION_LABEL`.
  - Imports always copy files into the Audiobookshelf library and remove the app label after import.
- Added a first‑run setup wizard:
  - `GET /` serves `setup.html` if `needs_setup()` (no cookie, no lib dir, or no path map) and setup is not disabled via env.
  - `GET /setup` shows the wizard unless `DISABLE_SETUP` is set (then it returns 404).
  - `POST /api/setup` writes `/data/config.json` and calls `settings.reload()`.
  - UI files: `app/templates/setup.html`, `app/static/setup.js`.
- Setup UX tweaks:
  - Main page includes a “Setup / Configuration” button (hidden when `DISABLE_SETUP` is enabled).
  - The setup page title links back to `/`.
- Root‑level `AGENTS.md` documents repo conventions and agent guidance.

## How to Run for Testing

- Local dev (no Docker), from `app/`:
  - `uvicorn main:app --reload --host 0.0.0.0 --port 8080`
  - Optionally set `APP_CONFIG_PATH=../dev-config.json` to avoid writing into `/data`.
- Docker (on Unraid or similar):
  - Update `.env` for mounts and ports, then `docker compose up -d`.
  - First visit to `/` on a fresh data directory should trigger the setup wizard (unless `DISABLE_SETUP` is set).

## Release Notes / Checklist (GHCR)

- Build and tag image from repo root:
  - `docker build -t ghcr.io/raygan/mam-audiofinder:0.6 -t ghcr.io/raygan/mam-audiofinder:latest .`
- Login to GHCR (once per machine):
  - `echo "$GHCR_PAT" | docker login ghcr.io -u raygan --password-stdin`
- Push tags:
  - `docker push ghcr.io/raygan/mam-audiofinder:0.6`
  - `docker push ghcr.io/raygan/mam-audiofinder:latest`
- Consumers update via:
  - `docker compose pull && docker compose up -d`

## Possible Next Steps

- Add a “Test Transmission connection” button on the setup page.
- Improve error messages when `map_transmission_path` cannot resolve a path.
- Add a minimal `pytest` suite that mocks MAM/Transmission and exercises `/health`, `/search`, `/add`, `/transmission/torrents`, and `/import` using a temp `/data` directory.
- Investigate adding real time download status for recently added torrents
- Investigate displaying artwork. Available via MAM API?
