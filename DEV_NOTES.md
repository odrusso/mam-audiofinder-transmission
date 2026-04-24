# Dev Notes – mam-audiofinder

## Recent Key Changes Implemented

- Added a `Settings` helper in `app/main.py`:
  - Loads config from `/data/config.json` (or `APP_CONFIG_PATH`) and falls back to env vars.
  - Centralizes: `MAM_COOKIE`, `TRANSMISSION_URL`, `TRANSMISSION_USER`, `TRANSMISSION_PASS`, `TRANSMISSION_LABEL`, etc.
- Standardized storage paths:
  - The app expects completed Transmission downloads under `/downloads` and imports into `/library`.
  - Docker Compose mounts host storage directly to those static in-container paths.
  - Imports reject Transmission paths outside `/downloads` with a clear mount mismatch error.
- Added Transmission RPC integration:
  - `POST /add` calls `torrent-add` using either a MAM direct URL or base64 `.torrent` upload.
  - `GET /transmission/torrents` calls `torrent-get`, returning completed torrents with `TRANSMISSION_LABEL`.
  - Imports always copy files into the Audiobookshelf library and remove the app label after import.
- Added a first‑run setup wizard:
  - `GET /` serves `setup.html` if `needs_setup()` (no MAM cookie) and setup is not disabled via env.
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
  - Update `.env` for runtime values and `docker-compose.yml` for `/downloads` and `/library` mounts, then `docker compose up -d`.
  - First visit to `/` on a fresh data directory should trigger the setup wizard (unless `DISABLE_SETUP` is set).

## Release Notes / Checklist (GHCR)

- Push to `main` or `master`; GitHub Actions auto-creates the next patch `vX.Y.Z` tag from the latest stable release tag.
- If no stable release tags exist yet, the first generated release is `v0.0.1`.
- GitHub Actions builds the image with generated `APP_VERSION` and publishes GHCR tags:
  - `latest` for the default branch
  - `main` or `master` for the branch ref
  - `sha-<commit>` for commit-pinned deploys
  - `vX.Y.Z`, `X.Y.Z`, and `X.Y` for release tags
- Consumers update via either a pinned `IMAGE_TAG` or:
  - `docker compose pull && docker compose up -d`

## Possible Next Steps

- Add a “Test Transmission connection” button on the setup page.
- Add a minimal `pytest` suite that mocks MAM/Transmission and exercises `/health`, `/search`, `/add`, `/transmission/torrents`, and `/import` using a temp `/data` directory.
- Investigate adding real time download status for recently added torrents
- Investigate displaying artwork. Available via MAM API?
