# Repository Guidelines

## Project Structure & Modules

- Backend FastAPI app lives in `app/main.py`.
- HTML templates are in `app/templates/` (`index.html`).
- Static assets (JS, icons, screenshots) are in `app/static/`.
- Container and environment files: `Dockerfile`, `docker-compose.yml`.
- SQLite data is expected under `/data` inside the container.

## Build, Run, and Development

- Local (no Docker, for quick checks):
  - From `app/`:  
    `uvicorn main:app --reload --host 0.0.0.0 --port 8080`
- Docker:
  - `docker compose up -d` – build and run using `.env`.
  - `docker compose build` – rebuild image after code changes.
- Basic syntax check:
  - `python -m py_compile app/main.py`

## Coding Style & Naming

- Python: 4‑space indentation, no tabs.
- Use `snake_case` for functions/variables, `CamelCase` for classes.
- Keep modules small and flat; prefer helpers in `main.py` over new packages unless needed.
- Frontend JS: modern ES syntax, avoid frameworks; keep logic in `app/static/app.js` or small new files.

## Testing Guidelines

- There is no formal test suite yet.
- When changing backend logic, at minimum:
  - Hit `/health`, `/search`, `/add`, and `/history` manually in a dev environment.
  - Verify auto-import behavior with a completed torrent in Transmission.

## Commit & Pull Request Guidelines

- Commits: small, focused, present‑tense messages, e.g. `Move runtime config to compose env`.
- Group related backend + frontend changes together when they implement one feature.
- PRs (or equivalent review units) should:
  - Describe the user‑visible change and any config/env vars added.
  - Include screenshots or GIFs for UI changes.

## Agent‑Specific Instructions

- Prefer minimal, surgical edits over wide refactors.
- Do not add new dependencies without updating `requirements.txt` and explaining why.
- Keep all runtime configuration behind environment variables in `docker-compose.yml`; do not hard‑code host‑specific paths.

## Config & Storage Notes (2025-12)

- Runtime config comes from environment variables passed by `docker-compose.yml`.
- Storage paths are static inside the app container:
  - Transmission downloads must be mounted at `/downloads`.
  - The Audiobookshelf library must be mounted at `/library`.
  - Configure host paths in `docker-compose.yml`, not through app env vars.
