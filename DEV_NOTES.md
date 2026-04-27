# Dev Notes – mam-audiofinder

## Recent Key Changes Implemented

- Rewrote the runtime from FastAPI/Python to Go:
  - New Go entrypoint is [`main.go`](/Users/oscar/Projects/mam-audiofinder-transmission/main.go).
  - Config still loads from `/data/config.json` by default, with `APP_CONFIG_PATH` and `APP_DATA_DIR` available for overrides.
  - SQLite history remains at `/data/history.db` by default, managed via the `sqlite3` CLI to avoid an external Go driver dependency.
- Kept the same UI and API surface:
  - Search, add, history, transmission torrent listing, and import endpoints still exist.
  - Templates were converted to Go `html/template`.
- Standardized storage paths:
  - The app expects completed Transmission downloads under `/downloads` and imports into `/library` or `/ebooks`.
  - Docker Compose mounts host storage directly to those static in-container paths.
- Docker and CI now build Go:
  - Dockerfile is a multi-stage Go build.
  - GitHub Actions runs `go test ./...` before building/pushing the container.

## How to Run for Testing

- Local dev (no Docker), from the repo root:
  - `APP_DATA_DIR=/tmp/mam-audiofinder-data go run .`
  - Optionally set `APP_CONFIG_PATH` to keep config separate from the default data dir.
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
