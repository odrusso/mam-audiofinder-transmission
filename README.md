# MAM Audiobook Finder

A lightweight web app + API to quickly search MyAnonamouse for audiobooks, add them to Transmission, and import completed downloads into your [Audiobookshelf](https://www.audiobookshelf.org/) library.

![Search](/app/static/screenshots/search.png)
![Import](/app/static/screenshots/import.png)


## Features

- **Search MAM** by title, author, or narrator  
- **One-click add to Transmission** (with its own label)
- **History view** of all books you've added  
- **Inline import tool** to copy completed downloads into your Audiobookshelf library
- Minimal, fast UI that works on desktop and mobile
- ZERO AUTHENTICATION (*Please* don't put this on the open internet. Tailscale or a Cloudflare Tunnel with Cloudflare Access might be good options.)
- Spouse tested and approved

## Requirements

- Transmission with RPC enabled and label support
- A valid MAM session cookie for setup or env/config fallback
- Docker & Docker Compose

## Because You'll Ask

***Why build this*** instead of using Readarr or one of its revivals, forks, or related projects?
- This uses the MAM API directly, not just for finding books but also for metadata. It doesn't rely on any other systems or databases. This is great for me because when a book shows up in my search results I KNOW I can download it, and I know the metadata like narrator and format will be accurate.
- I wanted something I could use from my phone that would be as fast as I could make it. This is very fast.
- This also keeps things dead simple. There's no queue, no requests, no ranking of multiple sources, no usenet, no RSS. Just search, download, and import to library.


## Quick Start

The checked-in `docker-compose.yml` supports both local source builds and the published GHCR image. The simplest first boot from a local checkout is:

1. Clone this repository and `cd` into it:
   ```bash
   git clone https://github.com/odrusso/mam-audiofinder-transmission.git
   cd mam-audiofinder-transmission
   ```
2. Copy `env.example` → `.env` and fill in the small set of runtime values:
   - App port and state directory (`APP_PORT`, `DATA_DIR`)
   - Container user/permissions (`PUID`, `PGID`, `UMASK`)
3. Create the host state directory from `DATA_DIR` and make sure it is writable by `PUID:PGID`.
4. Edit the `volumes` section in `docker-compose.yml` so your host storage is mounted at the app's static in-container paths. Replace the placeholder paths on the left before starting:
   - Transmission downloads at `/downloads`
   - Audiobookshelf library at `/library`
5. If Transmission runs in Docker, mount the same host downloads directory into the Transmission container too, so Transmission reports completed downloads under `/downloads`.
6. Start the container with Docker Compose:
   ```bash
   docker compose up -d --build
   ```
7. Visit [http://localhost:8008](http://localhost:8008) or your chosen `APP_PORT`. On first run, the home page will show the setup screen. Enter your MAM cookie, Transmission RPC URL, Transmission credentials if needed, and the label to use for new torrents.
8. Settings are saved to `/data/config.json`. After you have finished setup, you can optionally set `DISABLE_SETUP=1` and restart the container to hide `/setup`. Do not enable `DISABLE_SETUP` before initial setup unless you are supplying the setup-backed settings through env vars or an existing config file.

If you want to use the published GHCR image instead of building locally, authenticate first if the package is private, then pull and start:

```bash
echo "$GHCR_PAT" | docker login ghcr.io -u <github-username> --password-stdin
docker compose pull
docker compose up -d
```

By default Compose uses `IMAGE_TAG=latest`. To pin a specific published release, set `IMAGE_TAG` in `.env`, for example:

```bash
IMAGE_TAG=0.3.0
```

To confirm the app is running, open the UI or check the health endpoint on your chosen app port:

```bash
curl http://localhost:8008/health
```

The health response includes the running app version.

## Environment Variables

The app is setup-first. MAM and Transmission settings are normally saved through `/setup` into `/data/config.json`; env vars are only fallbacks.

Runtime/env-only values:

| Variable          | Description                                                                 |
|-------------------|-----------------------------------------------------------------------------|
| `APP_PORT`        | Host port that exposes the app's web UI (used in `docker-compose.yml`)      |
| `IMAGE_TAG`       | Published GHCR image tag for Compose pulls, default `latest`                |
| `DATA_DIR`        | Host path where this app stores `/data` state, including config and SQLite  |
| `PUID`            | Container user ID (for file permissions, default `99`)                      |
| `PGID`            | Container group ID (for file permissions, default `100`)                    |
| `UMASK`           | File creation mask (default `0002`)                                         |
| `DISABLE_SETUP`   | When set to `1`/`true`, hides the setup button and disables `/setup` and `/api/setup` |
| `APP_CONFIG_PATH` | Optional advanced override for the config JSON path                         |

Setup-backed values, also supported as env fallbacks:

| Variable             | Description                                                             |
|----------------------|-------------------------------------------------------------------------|
| `MAM_BASE`           | Optional advanced override for the MAM base URL                         |
| `MAM_COOKIE`         | Your MAM session cookie (use ASN-locked cookie)                         |
| `TRANSMISSION_URL`   | Transmission RPC URL (e.g. `http://transmission:9091/transmission/rpc`) |
| `TRANSMISSION_USER`  | Transmission RPC username, if auth is enabled                           |
| `TRANSMISSION_PASS`  | Transmission RPC password, if auth is enabled                           |
| `TRANSMISSION_LABEL` | Label assigned to new torrents and used for import filtering            |

## Storage configuration examples

The app uses fixed in-container paths and does not read path settings from env or setup:

- `/downloads` for Transmission downloads
- `/library` for the Audiobookshelf library

This app expects Transmission's completed downloads to be visible at `/downloads`. Configure Transmission's default download directory and mounts so it reports paths under `/downloads`. Imports always copy files from `/downloads` into `/library`.

### 1. Single media root

If your downloads and library live under a common parent, mount each subdirectory to the static app path.

Example host layout:

- Transmission downloads: `/mnt/media/torrents`
- Audiobookshelf: `/mnt/media/audiobookshelf`

`docker-compose.yml`:

```yaml
volumes:
  - ${DATA_DIR}:/data
  - /mnt/media/torrents:/downloads
  - /mnt/media/audiobookshelf:/library
```

### 2. Separate mounts (downloads and library on different paths)

If your downloads and library are on different host paths, still keep the in-container paths static.

Example host layout:

- Transmission downloads: `/mnt/disk1/torrents`
- Audiobookshelf: `/mnt/disk2/audiobooks`

`docker-compose.yml` (adjust or override the `volumes` section):

```yaml
volumes:
  - ${DATA_DIR}:/data
  - /mnt/disk1/torrents:/downloads
  - /mnt/disk2/audiobooks:/library
```

## Versioning and releases

The app version is not stored in source. GitHub Actions generates it when code is pushed to the default branch.

On each push to `main` or `master`, the publish workflow finds the latest stable `vX.Y.Z` git tag, increments the patch number, creates the next tag on that commit, and builds the image with that generated version. If no stable release tags exist yet, the first release is `0.0.1`.

The GHCR workflow publishes these image tags:

- `latest` for the default branch build
- `main` or `master` for the branch ref
- `sha-<commit>` for commit-pinned deploys
- `vX.Y.Z`, `X.Y.Z`, and `X.Y` for release tags

Published images receive `APP_VERSION` at build time. Local source runs that were not built by CI report `unknown`.


This project was created to scratch a personal itch, and was almost entirely vibe-coded with OpenAI Codex. I will probably not be developing it further, looking at issues, or accepting pull requests.
Do not run this on the open internet! 
Are you a *real* developer? Do you want to fork or rewrite this project and make it not suck? Go for it!

## License

MIT — provided as-is, no warranty.
