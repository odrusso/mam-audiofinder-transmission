import os, json, re, base64, asyncio, logging
from pathlib import Path
import shutil
import httpx
from fastapi import FastAPI, Request, HTTPException
from fastapi.responses import HTMLResponse, JSONResponse
from fastapi.staticfiles import StaticFiles
from fastapi.templating import Jinja2Templates
from pydantic import BaseModel
from sqlalchemy import create_engine, text
from datetime import datetime

logger = logging.getLogger("mam_audiofinder")

# ---------------------------- Config ----------------------------
CONFIG_PATH = os.getenv("APP_CONFIG_PATH", "/data/config.json")
DOWNLOADS_DIR = "/downloads"
LIBRARY_DIR = "/library"
DEFAULT_AUTO_IMPORT_POLL_INTERVAL = 30

APP_VERSION = os.getenv("APP_VERSION", "unknown")

def load_json_config() -> dict:
    try:
        with open(CONFIG_PATH, "r", encoding="utf-8") as f:
            data = json.load(f)
            return data if isinstance(data, dict) else {}
    except FileNotFoundError:
        return {}
    except Exception:
        return {}

def is_truthy(value) -> bool:
    if isinstance(value, bool):
        return value
    return str(value).strip().lower() in ("1", "true", "yes", "on")

def parse_positive_int(value, default: int) -> int:
    try:
        parsed = int(value)
    except (TypeError, ValueError):
        return default
    return parsed if parsed > 0 else default

def is_setup_disabled() -> bool:
    return is_truthy(os.getenv("DISABLE_SETUP", ""))

def build_mam_cookie(raw: str) -> str:
    raw = (raw or "").strip()
    if not raw:
        return ""
    # If user pasted full cookie header, use it as-is
    if "mam_id=" in raw or "mam_session=" in raw:
        return raw
    # If ASN single-token was pasted, wrap it
    if raw and "=" not in raw and ";" not in raw:
        return f"mam_id={raw}"
    return raw

class Settings:
    def __init__(self) -> None:
        self.reload()

    def reload(self) -> None:
        cfg = load_json_config()

        self.MAM_BASE = cfg.get("MAM_BASE") or os.getenv("MAM_BASE", "https://www.myanonamouse.net")

        raw_cookie = cfg.get("MAM_COOKIE")
        if raw_cookie is None:
            raw_cookie = os.getenv("MAM_COOKIE", "")
        self.MAM_COOKIE = build_mam_cookie(raw_cookie)

        self.TRANSMISSION_URL = (
            cfg.get("TRANSMISSION_URL")
            or os.getenv("TRANSMISSION_URL", "http://transmission:9091/transmission/rpc")
        ).rstrip("/")
        self.TRANSMISSION_USER = cfg.get("TRANSMISSION_USER") or os.getenv("TRANSMISSION_USER", "")
        self.TRANSMISSION_PASS = cfg.get("TRANSMISSION_PASS") or os.getenv("TRANSMISSION_PASS", "")
        self.TRANSMISSION_LABEL = cfg.get("TRANSMISSION_LABEL") or os.getenv("TRANSMISSION_LABEL", "mam-audiofinder")
        self.DOWNLOADS_DIR = DOWNLOADS_DIR
        self.LIBRARY_DIR = LIBRARY_DIR

        self.UMASK = cfg.get("UMASK") or os.getenv("UMASK")
        auto_import_enabled = cfg["AUTO_IMPORT_ENABLED"] if "AUTO_IMPORT_ENABLED" in cfg else os.getenv("AUTO_IMPORT_ENABLED", "")
        self.AUTO_IMPORT_ENABLED = is_truthy(auto_import_enabled)
        self.AUTO_IMPORT_POLL_INTERVAL = parse_positive_int(
            os.getenv("AUTO_IMPORT_POLL_INTERVAL"),
            DEFAULT_AUTO_IMPORT_POLL_INTERVAL,
        )

settings = Settings()

# apply UMASK for created files/dirs
_um = settings.UMASK
if _um:
    try:
        os.umask(int(_um, 8))
    except Exception:
        pass

# ---------------------------- DB ----------------------------
# /data should be a volume/bind mount
engine = create_engine("sqlite:////data/history.db", future=True)

def utcnow_str() -> str:
    return datetime.utcnow().strftime("%Y-%m-%d %H:%M:%S")

def ensure_history_schema() -> None:
    with engine.begin() as cx:
        cx.execute(text("""
            CREATE TABLE IF NOT EXISTS history (
              id INTEGER PRIMARY KEY,
              mam_id   TEXT,
              title    TEXT,
              author   TEXT,
              narrator TEXT,
              dl       TEXT,
              added_at TEXT DEFAULT (datetime('now')),
              imported_at TEXT,
              torrent_status TEXT,
              torrent_hash   TEXT
            )
        """))
        cols = {row["name"] for row in cx.execute(text("PRAGMA table_info(history)")).mappings()}
        if "status_detail" not in cols:
            cx.execute(text("ALTER TABLE history ADD COLUMN status_detail TEXT"))
        if "status_updated_at" not in cols:
            cx.execute(text("ALTER TABLE history ADD COLUMN status_updated_at TEXT"))
        cx.execute(text("""
            UPDATE history
            SET torrent_status = 'added'
            WHERE torrent_status IS NULL OR trim(torrent_status) = ''
        """))
        cx.execute(text("""
            UPDATE history
            SET status_updated_at = COALESCE(status_updated_at, imported_at, added_at)
            WHERE status_updated_at IS NULL
        """))

ensure_history_schema()

def needs_setup() -> bool:
    return not settings.MAM_COOKIE

def setup_context(request: Request) -> dict:
    return {
        "request": request,
        "app_version": APP_VERSION,
        "transmission_url": settings.TRANSMISSION_URL,
        "transmission_user": settings.TRANSMISSION_USER,
        "transmission_label": settings.TRANSMISSION_LABEL,
        "auto_import_enabled": settings.AUTO_IMPORT_ENABLED,
    }

class SetupPayload(BaseModel):
    mam_cookie: str | None = None
    transmission_url: str | None = None
    transmission_user: str | None = None
    transmission_pass: str | None = None
    transmission_label: str | None = None
    auto_import_enabled: bool | None = None

# ---------------------------- App ----------------------------
app = FastAPI(title="MAM Audiobook Finder", version=APP_VERSION)
app.state.auto_import_task = None
app.state.auto_import_stop = None

app.mount("/static", StaticFiles(directory="static"), name="static")
templates = Jinja2Templates(directory="templates")

@app.get("/health")
async def health():
    return {"ok": True, "version": APP_VERSION}

@app.get("/", response_class=HTMLResponse)
async def home(request: Request):
    setup_enabled = not is_setup_disabled()
    if needs_setup() and setup_enabled:
        return templates.TemplateResponse(
            request=request,
            name="setup.html",
            context=setup_context(request),
        )
    return templates.TemplateResponse(
        request=request,
        name="index.html",
        context={"request": request, "app_version": APP_VERSION, "setup_enabled": setup_enabled},
    )

@app.get("/setup", response_class=HTMLResponse)
async def setup_page(request: Request):
    if is_setup_disabled():
        raise HTTPException(status_code=404, detail="Not found")
    return templates.TemplateResponse(
        request=request,
        name="setup.html",
        context=setup_context(request),
    )

@app.post("/api/setup")
async def api_setup(body: SetupPayload):
    if is_setup_disabled():
        raise HTTPException(status_code=404, detail="Not found")
    cfg = load_json_config()
    if not isinstance(cfg, dict):
        cfg = {}

    if body.mam_cookie and body.mam_cookie.strip():
        cfg["MAM_COOKIE"] = body.mam_cookie.strip()
    if body.transmission_url and body.transmission_url.strip():
        cfg["TRANSMISSION_URL"] = body.transmission_url.strip()
    if body.transmission_user and body.transmission_user.strip():
        cfg["TRANSMISSION_USER"] = body.transmission_user.strip()
    if body.transmission_pass:
        cfg["TRANSMISSION_PASS"] = body.transmission_pass
    if body.transmission_label and body.transmission_label.strip():
        cfg["TRANSMISSION_LABEL"] = body.transmission_label.strip()
    if body.auto_import_enabled is not None:
        cfg["AUTO_IMPORT_ENABLED"] = bool(body.auto_import_enabled)

    # Persist config
    try:
        dirpath = os.path.dirname(CONFIG_PATH)
        if dirpath:
            os.makedirs(dirpath, exist_ok=True)
        with open(CONFIG_PATH, "w", encoding="utf-8") as f:
            json.dump(cfg, f, indent=2)
    except Exception as e:
        raise HTTPException(status_code=500, detail=f"Failed to write config: {e}")

    settings.reload()
    await reconcile_auto_import_task()
    return {"ok": True}

# ---------------------------- Search ----------------------------
@app.post("/search")
async def search(payload: dict):
    if not settings.MAM_COOKIE:
        raise HTTPException(status_code=500, detail="MAM_COOKIE not set on server")

    tor = payload.get("tor", {}) or {}
    tor.setdefault("text", "")
    tor.setdefault("srchIn", ["title", "author", "narrator"])
    tor.setdefault("searchType", "all")
    tor["sortType"] = "seedersDesc"
    tor.setdefault("startNumber", "0")
    tor.setdefault("main_cat", ["13"])  # Audiobooks

    perpage = payload.get("perpage", 25)
    body = {"tor": tor, "perpage": perpage}

    headers = {
        "Cookie": settings.MAM_COOKIE,
        "Content-Type": "application/json",
        "Accept": "application/json, */*",
        "User-Agent": "Mozilla/5.0",
        "Origin": "https://www.myanonamouse.net",
        "Referer": "https://www.myanonamouse.net/",
    }
    params = {"dlLink": "1"}

    try:
        async with httpx.AsyncClient(timeout=30) as client:
            r = await client.post(f"{settings.MAM_BASE}/tor/js/loadSearchJSONbasic.php",
                                  headers=headers, params=params, json=body)
    except httpx.HTTPError as e:
        raise HTTPException(status_code=502, detail=f"MAM request failed: {e}")

    if r.status_code != 200:
        raise HTTPException(status_code=502, detail=f"MAM HTTP {r.status_code}: {r.text[:300]}")
    try:
        raw = r.json()
    except ValueError:
        raise HTTPException(status_code=502, detail=f"MAM returned non-JSON. Body: {r.text[:300]}")

    def flatten(v):
        # {"8320":"John Steinbeck"} or JSON-string -> "John Steinbeck"
        if isinstance(v, dict):
            return ", ".join(str(x) for x in v.values())
        if isinstance(v, list):
            return ", ".join(str(x) for x in v)
        if isinstance(v, str):
            s = v.strip()
            if s.startswith("{") or s.startswith("["):
                try:
                    obj = json.loads(s)
                    if isinstance(obj, dict):
                        return ", ".join(str(x) for x in obj.values())
                    if isinstance(obj, list):
                        return ", ".join(str(x) for x in obj)
                except Exception:
                    pass
            s = re.sub(r'^\{|\}$', '', s)
            parts = []
            for chunk in s.split(","):
                parts.append(chunk.split(":", 1)[-1])
            parts = [p.strip().strip('"').strip("'") for p in parts if p.strip()]
            return ", ".join(parts)
        return "" if v is None else str(v)

    def detect_format(item: dict) -> str:
        for key in ("format", "filetype", "container", "encoding", "format_name"):
            val = item.get(key)
            if isinstance(val, str) and val.strip():
                return val.strip()
        name = (item.get("title") or item.get("name") or "")
        toks = re.findall(r'(?i)\b(mp3|m4b|flac|aac|ogg|opus|wav|alac|ape|epub|pdf|mobi|azw3|cbz|cbr)\b', name)
        if toks:
            uniq = list(dict.fromkeys(t.upper() for t in toks))
            return "/".join(uniq)
        return ""

    out = []
    for item in raw.get("data", []):
        is_freeleech = is_truthy(item.get("free")) or is_truthy(item.get("fl_vip"))
        is_vip = is_truthy(item.get("vip")) or is_truthy(item.get("fl_vip"))
        out.append({
            "id": str(item.get("id") or item.get("tid") or ""),
            "title": item.get("title") or item.get("name"),
            "author_info": flatten(item.get("author_info")),
            "narrator_info": flatten(item.get("narrator_info")),
            "format": detect_format(item),
            "size": item.get("size"),
            "seeders": item.get("seeders"),
            "leechers": item.get("leechers"),
            "catname": item.get("catname"),
            "added": item.get("added"),
            "dl": item.get("dl"),
            "is_freeleech": is_freeleech,
            "is_vip": is_vip,
        })

    return JSONResponse({
        "results": out,
        "total": raw.get("total"),
        "total_found": raw.get("total_found"),
    })

# ---------------------------- Transmission RPC helpers ----------------------------
def transmission_auth():
    if settings.TRANSMISSION_USER or settings.TRANSMISSION_PASS:
        return (settings.TRANSMISSION_USER, settings.TRANSMISSION_PASS)
    return None

async def transmission_rpc(client: httpx.AsyncClient, method: str, arguments: dict | None = None) -> dict:
    payload = {"method": method, "arguments": arguments or {}}
    r = await client.post(settings.TRANSMISSION_URL, json=payload, auth=transmission_auth())
    if r.status_code == 409:
        session_id = r.headers.get("X-Transmission-Session-Id")
        if session_id:
            client.headers["X-Transmission-Session-Id"] = session_id
            r = await client.post(settings.TRANSMISSION_URL, json=payload, auth=transmission_auth())
    if r.status_code != 200:
        raise HTTPException(status_code=502, detail=f"Transmission RPC failed: {r.status_code} {r.text[:160]}")
    try:
        data = r.json()
    except ValueError:
        raise HTTPException(status_code=502, detail=f"Transmission returned non-JSON: {r.text[:160]}")
    if data.get("result") != "success":
        raise HTTPException(status_code=502, detail=f"Transmission {method} failed: {data.get('result')}")
    return data.get("arguments") or {}

def transmission_labels(mam_id: str = "") -> list[str]:
    labels = []
    if settings.TRANSMISSION_LABEL:
        labels.append(settings.TRANSMISSION_LABEL)
    if mam_id:
        labels.append(f"mamid={mam_id}")
    return labels

def torrent_add_arguments(mam_id: str, source_key: str, source_value: str) -> dict:
    args = {source_key: source_value}
    labels = transmission_labels(mam_id)
    if labels:
        args["labels"] = labels
    return args

def torrent_hash_from_add_result(args: dict) -> str | None:
    torrent = args.get("torrent-added") or args.get("torrent-duplicate") or {}
    return torrent.get("hashString")

def insert_history(mam_id: str, title: str, author: str, narrator: str, dl: str, torrent_hash: str | None):
    added_at = utcnow_str()
    with engine.begin() as cx:
        cx.execute(text("""
            INSERT INTO history (
                mam_id,
                title,
                author,
                narrator,
                dl,
                torrent_status,
                torrent_hash,
                added_at,
                status_detail,
                status_updated_at
            )
            VALUES (
                :mam_id,
                :title,
                :author,
                :narrator,
                :dl,
                :torrent_status,
                :torrent_hash,
                :added_at,
                :status_detail,
                :status_updated_at
            )
        """), {
            "mam_id": mam_id,
            "title": title,
            "author": author,
            "narrator": narrator,
            "dl": dl,
            "torrent_status": "added",
            "torrent_hash": torrent_hash,
            "added_at": added_at,
            "status_detail": None,
            "status_updated_at": added_at,
        })

# ---------------------------- Add-to-Transmission ----------------------------
class AddBody(BaseModel):
    id: str | int | None = None
    title: str | None = None
    dl: str | None = None
    author: str | None = None
    narrator: str | None = None

@app.post("/add")
async def add_to_transmission(body: AddBody):
    mam_id = ("" if body.id is None else str(body.id)).strip()
    title = (body.title or "").strip()
    author = (body.author or "").strip()
    narrator = (body.narrator or "").strip()
    dl = (body.dl or "").strip()

    if not mam_id and not dl:
        raise HTTPException(status_code=400, detail="Missing MAM id and dl; need at least one")

    direct_url = f"{settings.MAM_BASE}/tor/download.php/{dl}" if dl else None
    id_candidates = []
    if mam_id:
        id_candidates = [
            f"{settings.MAM_BASE}/tor/download.php?id={mam_id}",
            f"{settings.MAM_BASE}/tor/download.php?tid={mam_id}",
        ]

    torrent_hash = None

    async with httpx.AsyncClient(timeout=60) as client:
        # Try URL add first if we have a cookie-less direct link
        if direct_url:
            try:
                args = await transmission_rpc(
                    client,
                    "torrent-add",
                    torrent_add_arguments(mam_id, "filename", direct_url),
                )
                torrent_hash = torrent_hash_from_add_result(args)
                insert_history(mam_id, title, author, narrator, dl, torrent_hash)
                return {"ok": True}
            except HTTPException:
                if not id_candidates:
                    raise
                # fall through to cookie-authenticated fetch/upload

        # Cookie-authenticated fetch of .torrent, then upload
        mam_headers = {
            "Cookie": settings.MAM_COOKIE,
            "User-Agent": "Mozilla/5.0",
            "Accept": "application/x-bittorrent, */*",
            "Referer": "https://www.myanonamouse.net/",
            "Origin": "https://www.myanonamouse.net",
        }
        torrent_bytes = None
        for url in id_candidates:
            resp = await client.get(url, headers=mam_headers)
            if resp.status_code == 200 and resp.content:
                torrent_bytes = resp.content
                break

        if not torrent_bytes:
            raise HTTPException(status_code=502, detail="Could not fetch .torrent from MAM (no dl hash and cookie fetch failed).")

        metainfo = base64.b64encode(torrent_bytes).decode("ascii")
        args = await transmission_rpc(
            client,
            "torrent-add",
            torrent_add_arguments(mam_id, "metainfo", metainfo),
        )
        torrent_hash = torrent_hash_from_add_result(args)
        insert_history(mam_id, title, author, narrator, dl, torrent_hash)

    return {"ok": True}

# ---------------------------- History ----------------------------
@app.get("/history")
def history():
    with engine.begin() as cx:
        rows = cx.execute(text("""
            SELECT
                id,
                mam_id,
                title,
                author,
                narrator,
                dl,
                torrent_hash,
                added_at,
                imported_at,
                torrent_status,
                status_detail,
                status_updated_at
            FROM history
            ORDER BY id DESC
            LIMIT 200
        """)).mappings().all()
    return {"items": list(rows)}

@app.delete("/history/{row_id}")
def delete_history(row_id: int):
    with engine.begin() as cx:
        cx.execute(text("DELETE FROM history WHERE id = :id"), {"id": row_id})
    return {"ok": True}
    
# ---------------------------- List Importable ----------------------------
async def list_completed_torrents() -> list[dict]:
    async with httpx.AsyncClient(timeout=30) as c:
        args = await transmission_rpc(c, "torrent-get", {
            "fields": [
                "id",
                "hashString",
                "name",
                "percentDone",
                "downloadDir",
                "totalSize",
                "addedDate",
                "labels",
                "files",
            ],
        })
        infos = args.get("torrents") or []

        out = []
        for t in infos:
            if settings.TRANSMISSION_LABEL and settings.TRANSMISSION_LABEL not in (t.get("labels") or []):
                continue
            if float(t.get("percentDone") or 0) < 1:
                continue

            h = t.get("hashString")
            if not h:
                continue
            files = t.get("files") or []
            # compute top-level root (before first '/')
            roots = set()
            for f in files:
                name = (f.get("name") or "").lstrip("/")
                roots.add(name.split("/", 1)[0])
            root = (list(roots)[0] if roots else t.get("name") or "")
            single_file = len(files) == 1 and "/" not in (files[0].get("name") or "")
            out.append({
                "hash": h,
                "name": t.get("name"),
                "download_dir": t.get("downloadDir"),
                "root": root,
                "single_file": single_file,
                "size": t.get("totalSize"),
                "added_on": t.get("addedDate"),
            })
        return out

@app.get("/transmission/torrents")
async def transmission_torrents():
    return {"items": await list_completed_torrents()}
        
# ---------------------------- Perform Import ----------------------------

def sanitize(name: str) -> str:
    s = name.strip().replace(":", " -").replace("\\", "﹨").replace("/", "﹨")
    return re.sub(r"\s+", " ", s)[:200] or "Unknown"

def next_available(path: Path) -> Path:
    if not path.exists():
        return path
    i = 2
    while True:
        cand = path.with_name(f"{path.name} ({i})")
        if not cand.exists():
            return cand
        i += 1

def copy_one(src: Path, dst: Path):
    dst.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(src, dst)

def clean_status_detail(detail: str | None) -> str | None:
    text_value = re.sub(r"\s+", " ", (detail or "").strip())
    return text_value[:500] or None

def update_history_status(history_id: int, status: str, detail: str | None = None, imported_at: str | None = None):
    ts = utcnow_str()
    with engine.begin() as cx:
        cx.execute(text("""
            UPDATE history
            SET
                torrent_status = :status,
                status_detail = :detail,
                status_updated_at = :status_updated_at,
                imported_at = COALESCE(:imported_at, imported_at)
            WHERE id = :id
        """), {
            "id": history_id,
            "status": status,
            "detail": clean_status_detail(detail),
            "status_updated_at": ts,
            "imported_at": imported_at,
        })

def mark_history_imported(history_id: int | None, torrent_hash: str):
    ts = utcnow_str()
    with engine.begin() as cx:
        if history_id is not None:
            cx.execute(text("""
                UPDATE history
                SET
                    torrent_status = 'imported',
                    status_detail = NULL,
                    status_updated_at = :ts,
                    imported_at = :ts
                WHERE id = :id
            """), {"ts": ts, "id": history_id})
        else:
            cx.execute(text("""
                UPDATE history
                SET
                    torrent_status = 'imported',
                    status_detail = NULL,
                    status_updated_at = :ts,
                    imported_at = :ts
                WHERE torrent_hash = :torrent_hash
            """), {"ts": ts, "torrent_hash": torrent_hash})

def mark_history_failed(history_id: int | None, torrent_hash: str, detail: str):
    with engine.begin() as cx:
        params = {"detail": clean_status_detail(detail)}
        if history_id is not None:
            params["id"] = history_id
            cx.execute(text("""
                UPDATE history
                SET
                    torrent_status = 'import_failed',
                    status_detail = :detail,
                    status_updated_at = :ts
                WHERE id = :id
            """), {"ts": utcnow_str(), **params})
        else:
            params["torrent_hash"] = torrent_hash
            cx.execute(text("""
                UPDATE history
                SET
                    torrent_status = 'import_failed',
                    status_detail = :detail,
                    status_updated_at = :ts
                WHERE torrent_hash = :torrent_hash
            """), {"ts": utcnow_str(), **params})

def get_auto_import_candidates(completed_hashes: set[str]) -> list[dict]:
    if not completed_hashes:
        return []
    with engine.begin() as cx:
        rows = cx.execute(text("""
            SELECT id, title, author, torrent_hash, torrent_status
            FROM history
            WHERE
                torrent_hash IS NOT NULL
                AND trim(torrent_hash) != ''
                AND (
                    torrent_status IS NULL
                    OR torrent_status NOT IN ('imported', 'import_failed', 'importing')
                )
            ORDER BY id ASC
        """)).mappings().all()
    out = []
    seen_hashes = set()
    for row in rows:
        torrent_hash = (row.get("torrent_hash") or "").strip()
        if not torrent_hash or torrent_hash not in completed_hashes or torrent_hash in seen_hashes:
            continue
        seen_hashes.add(torrent_hash)
        out.append(dict(row))
    return out

def validate_download_path(p: str) -> str:
    p = (p or "").strip()
    if not p:
        return p
    downloads_dir = settings.DOWNLOADS_DIR.rstrip("/") or "/"
    if p == downloads_dir or p.startswith(downloads_dir + "/"):
        return p
    raise HTTPException(
        status_code=400,
        detail=(
            f"Transmission reports downloadDir '{p}', but this app expects completed "
            f"downloads under {settings.DOWNLOADS_DIR}. Mount the same downloads "
            f"directory at {settings.DOWNLOADS_DIR} in both containers."
        ),
    )

def is_transient_auto_import_error(exc: HTTPException) -> bool:
    detail = str(exc.detail)
    return exc.status_code == 502 and detail.startswith("Transmission")

class ImportBody(BaseModel):
    author: str
    title: str
    hash: str
    history_id: int | None = None

async def import_torrent_to_library(author: str, title: str, h: str) -> str:
    author = sanitize(author)
    title = sanitize(title)
    # Query Transmission for files and download directory.
    async with httpx.AsyncClient(timeout=30) as c:
        args = await transmission_rpc(c, "torrent-get", {
            "ids": [h],
            "fields": ["id", "hashString", "name", "downloadDir", "labels", "files"],
        })
        torrents = args.get("torrents") or []
        info = torrents[0] if torrents else {}
        files = info.get("files") or []
        if not files:
            raise HTTPException(status_code=404, detail="No files found for torrent")

        download_dir = (info.get("downloadDir") or "").rstrip("/")
        if not download_dir:
            raise HTTPException(status_code=404, detail="Torrent download directory not found")

    source_dir = Path(validate_download_path(download_dir))

    # Destination: /library/Author/Title[/...]
    lib = Path(settings.LIBRARY_DIR)
    author_dir = lib / author
    author_dir.mkdir(parents=True, exist_ok=True)
    dest_dir = next_available(author_dir / title)

    names = [(f.get("name") or "").lstrip("/") for f in files if f.get("name")]
    roots = {name.split("/", 1)[0] for name in names if "/" in name}
    common_root = next(iter(roots)) if len(roots) == 1 and all(name == next(iter(roots)) or name.startswith(next(iter(roots)) + "/") for name in names) else ""

    # Copy all files (skip .cue).
    copied = 0
    if len(names) == 1:
        src = source_dir / names[0]
        if src.suffix.lower() == ".cue":
            raise HTTPException(status_code=400, detail="Only .cue file found; nothing to import")
        copy_one(src, dest_dir / src.name)
        copied += 1
    else:
        for name in names:
            src = source_dir / name
            if src.suffix.lower() == ".cue":
                continue
            rel_name = name
            if common_root and name.startswith(common_root + "/"):
                rel_name = name[len(common_root) + 1:]
            if not rel_name:
                continue
            copy_one(src, dest_dir / rel_name)
            copied += 1

    if copied == 0:
        raise HTTPException(status_code=400, detail="No importable files found")

    return str(dest_dir)

@app.post("/import")
async def do_import(body: ImportBody):
    history_id = body.history_id
    if history_id is not None:
        update_history_status(history_id, "importing")

    try:
        dest = await import_torrent_to_library(body.author, body.title, body.hash)
    except HTTPException as exc:
        if history_id is not None:
            mark_history_failed(history_id, body.hash, str(exc.detail))
        raise
    except Exception as exc:
        logger.exception("Import failed for torrent %s", body.hash)
        detail = f"Import failed: {exc}"
        if history_id is not None:
            mark_history_failed(history_id, body.hash, detail)
        raise HTTPException(status_code=500, detail=detail)

    mark_history_imported(history_id, body.hash)
    return {"ok": True, "dest": dest}

async def auto_import_cycle():
    completed = await list_completed_torrents()
    completed_hashes = {item.get("hash") for item in completed if item.get("hash")}
    for row in get_auto_import_candidates(completed_hashes):
        history_id = row["id"]
        torrent_hash = (row.get("torrent_hash") or "").strip()
        author = (row.get("author") or "").strip()
        title = (row.get("title") or "").strip()

        if not author or not title:
            mark_history_failed(history_id, torrent_hash, "History row is missing author/title; use manual import.")
            continue

        update_history_status(history_id, "importing")

        try:
            await import_torrent_to_library(author, title, torrent_hash)
        except HTTPException as exc:
            if is_transient_auto_import_error(exc):
                update_history_status(history_id, "added")
                logger.warning("Auto-import skipped for history row %s: %s", history_id, exc.detail)
            else:
                mark_history_failed(history_id, torrent_hash, str(exc.detail))
                logger.warning("Auto-import failed for history row %s: %s", history_id, exc.detail)
            continue
        except Exception as exc:
            logger.exception("Unexpected auto-import failure for history row %s", history_id)
            mark_history_failed(history_id, torrent_hash, f"Import failed: {exc}")
            continue

        mark_history_imported(history_id, torrent_hash)
        logger.info("Auto-imported history row %s", history_id)

async def auto_import_loop(stop_event: asyncio.Event):
    logger.info("Auto-import poller started with %ss interval", settings.AUTO_IMPORT_POLL_INTERVAL)
    while not stop_event.is_set():
        try:
            await auto_import_cycle()
        except HTTPException as exc:
            logger.warning("Auto-import cycle skipped: %s", exc.detail)
        except Exception:
            logger.exception("Auto-import poller cycle failed")

        try:
            await asyncio.wait_for(stop_event.wait(), timeout=settings.AUTO_IMPORT_POLL_INTERVAL)
        except asyncio.TimeoutError:
            continue
    logger.info("Auto-import poller stopped")

async def stop_auto_import_task():
    task = getattr(app.state, "auto_import_task", None)
    stop_event = getattr(app.state, "auto_import_stop", None)
    if stop_event is not None:
        stop_event.set()
    if task is not None:
        try:
            await task
        except asyncio.CancelledError:
            pass
    app.state.auto_import_task = None
    app.state.auto_import_stop = None

async def reconcile_auto_import_task():
    task = getattr(app.state, "auto_import_task", None)
    if settings.AUTO_IMPORT_ENABLED:
        if task is None or task.done():
            stop_event = asyncio.Event()
            app.state.auto_import_stop = stop_event
            app.state.auto_import_task = asyncio.create_task(auto_import_loop(stop_event))
    elif task is not None:
        await stop_auto_import_task()

@app.on_event("startup")
async def startup_event():
    await reconcile_auto_import_task()

@app.on_event("shutdown")
async def shutdown_event():
    await stop_auto_import_task()
