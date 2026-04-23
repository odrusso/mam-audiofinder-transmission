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
- A valid MAM session cookie  
- Docker & Docker Compose

## Because You'll Ask

***Why build this*** instead of using Readarr or one of its revivals, forks, or related projects?
- This uses the MAM API directly, not just for finding books but also for metadata. It doesn't rely on any other systems or databases. This is great for me because when a book shows up in my search results I KNOW I can download it, and I know the metadata like narrator and format will be accurate.
- I wanted something I could use from my phone that would be as fast as I could make it. This is very fast.
- This also keeps things dead simple. There's no queue, no requests, no ranking of multiple sources, no usenet, no RSS. Just search, download, and import to library.


## Quick Start

This repository includes a `docker-compose.yml` for Docker users. The usual flow is:

1. Clone this repository and `cd` into it:
   ```bash
   git clone https://github.com/raygan/mam-audiofinder.git
   cd mam-audiofinder
   ```
2. Copy `.env.example` → `.env` and fill in the required **env-only** values:
   - App port and host paths (`APP_PORT`, `DATA_DIR`, and `MEDIA_ROOT`)
   - Container user/permissions (`PUID`, `PGID`, `UMASK`)
   - You can either set MAM/Transmission details here (`MAM_COOKIE`, `TRANSMISSION_URL`, `TRANSMISSION_USER`, `TRANSMISSION_PASS`) or leave them commented out and fill them in later via the web setup UI.
3. Mount the download path and Audiobookshelf library path into the app container. If you prefer separate mounts instead of a single `MEDIA_ROOT`, adjust the `volumes` section in `docker-compose.yml` as shown in the storage examples below, and set `DL_DIR` / `LIB_DIR` in `.env` to match.
4. Start the container with Docker Compose:
   ```bash
   docker compose up -d
   ```
5. Visit [http://localhost:8008](http://localhost:8008) (or your mapped port). On first run, you should land on `/setup` to configure MAM, Transmission, and library paths.

## Environment Variables

| Variable               | Description                                                                 |
|------------------------|-----------------------------------------------------------------------------|
| `MAM_COOKIE`           | Your MAM session cookie (use ASN-locked cookie)                             |
| `TRANSMISSION_URL`     | Transmission RPC URL (e.g. `http://transmission:9091/transmission/rpc`)     |
| `TRANSMISSION_USER`    | Transmission RPC username, if auth is enabled                              |
| `TRANSMISSION_PASS`    | Transmission RPC password, if auth is enabled                              |
| `APP_PORT`             | Host port that exposes the app’s web UI (used in `docker-compose.yml`)     |
| `MEDIA_ROOT`           | Host path mounted at `/media` inside the container                          |
| `DATA_DIR`             | Host path where this app stores its state (e.g. SQLite DB)                  |
| `DL_DIR`               | In-container path for Transmission downloads (default `/media/torrents`)    |
| `LIB_DIR`              | In-container path for Audiobookshelf library (default `/media/audiobookshelf`) |
| `TRANSMISSION_DOWNLOAD_DIR` | Optional explicit download directory sent to Transmission when adding torrents |
| `TRANSMISSION_LABEL`   | Label assigned to new torrents and used for import filtering (default `mam-audiofinder`) |
| `TRANSMISSION_INNER_DL_PREFIX` | Transmission’s internal download path prefix (default `/downloads`)   |
| `TRANSMISSION_PATH_MAP` | Optional env form of Transmission → app path mapping (`/downloads=/media/torrents;…`) |
| `PUID`                 | Container user ID (for file permissions, default `99`)                      |
| `PGID`                 | Container group ID (for file permissions, default `100`)                    |
| `UMASK`                | File creation mask (default `0002`)                                         |
| `DISABLE_SETUP`        | When set to `1`/`true`, hides the setup button and disables `/setup` and `/api/setup` after initial configuration |

## Storage configuration examples

The app only cares about the in-container paths `DL_DIR` (Transmission downloads) and `LIB_DIR` (Audiobookshelf library). Imports always copy files. How you mount host paths into the container is up to you. For example:

### 1. Single media root

If your downloads and library live under a common parent, you can use the default single `MEDIA_ROOT` mount.

Example host layout:

- Transmission downloads: `/mnt/media/torrents`
- Audiobookshelf: `/mnt/media/audiobookshelf`

`.env`:

```env
MEDIA_ROOT=/mnt/media
DL_DIR=/media/torrents
LIB_DIR=/media/audiobookshelf
```

`docker-compose.yml` (default) already contains:

```yaml
volumes:
  - ${DATA_DIR}:/data
  - ${MEDIA_ROOT}:/media
```

### 2. Separate mounts (downloads and library on different paths)

If your downloads and library are on different host paths (for example, separate disks/volumes, or you don’t want to mount a large media tree), you must update `docker-compose.yml` to mount each path explicitly and then point `DL_DIR` / `LIB_DIR` at those in-container locations. In this setup, `MEDIA_ROOT` is not used.

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

`.env`:

```env
DL_DIR=/downloads
LIB_DIR=/library
# MEDIA_ROOT is unused in this layout
```


This project was created to scratch a personal itch, and was almost entirely vibe-coded with OpenAI Codex. I will probably not be developing it further, looking at issues, or accepting pull requests.
Do not run this on the open internet! 
Are you a *real* developer? Do you want to fork or rewrite this project and make it not suck? Go for it!

## License

MIT — provided as-is, no warranty.
