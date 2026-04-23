import os, json, re, base64
import httpx
from fastapi import FastAPI, Request, HTTPException
from fastapi.responses import HTMLResponse, JSONResponse
from fastapi.staticfiles import StaticFiles
from fastapi.templating import Jinja2Templates
from pydantic import BaseModel
from sqlalchemy import create_engine, text
from datetime import datetime

# ---------------------------- Config ----------------------------
CONFIG_PATH = os.getenv("APP_CONFIG_PATH", "/data/config.json")
DOWNLOADS_DIR = "/downloads"
LIBRARY_DIR = "/library"

def load_json_config() -> dict:
    try:
        with open(CONFIG_PATH, "r", encoding="utf-8") as f:
            data = json.load(f)
            return data if isinstance(data, dict) else {}
    except FileNotFoundError:
        return {}
    except Exception:
        return {}

def is_setup_disabled() -> bool:
    val = os.getenv("DISABLE_SETUP", "")
    return str(val).strip().lower() in ("1", "true", "yes", "on")

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

def needs_setup() -> bool:
    return not settings.MAM_COOKIE

def setup_context(request: Request) -> dict:
    return {
        "request": request,
        "transmission_url": settings.TRANSMISSION_URL,
        "transmission_user": settings.TRANSMISSION_USER,
        "transmission_label": settings.TRANSMISSION_LABEL,
    }

class SetupPayload(BaseModel):
    mam_cookie: str | None = None
    transmission_url: str | None = None
    transmission_user: str | None = None
    transmission_pass: str | None = None
    transmission_label: str | None = None

# ---------------------------- App ----------------------------
app = FastAPI(title="MAM Audiobook Finder", version="0.3.0")

app.mount("/static", StaticFiles(directory="static"), name="static")
templates = Jinja2Templates(directory="templates")

@app.get("/health")
async def health():
    return {"ok": True}

@app.get("/", response_class=HTMLResponse)
async def home(request: Request):
    setup_enabled = not is_setup_disabled()
    if needs_setup() and setup_enabled:
        return templates.TemplateResponse("setup.html", setup_context(request))
    return templates.TemplateResponse("index.html", {"request": request, "setup_enabled": setup_enabled})

@app.get("/setup", response_class=HTMLResponse)
async def setup_page(request: Request):
    if is_setup_disabled():
        raise HTTPException(status_code=404, detail="Not found")
    return templates.TemplateResponse("setup.html", setup_context(request))

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
    tor.setdefault("sortType", "default")
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

async def transmission_rpc_async(client: httpx.AsyncClient, method: str, arguments: dict | None = None) -> dict:
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

def transmission_rpc_sync(client: httpx.Client, method: str, arguments: dict | None = None) -> dict:
    payload = {"method": method, "arguments": arguments or {}}
    r = client.post(settings.TRANSMISSION_URL, json=payload, auth=transmission_auth())
    if r.status_code == 409:
        session_id = r.headers.get("X-Transmission-Session-Id")
        if session_id:
            client.headers["X-Transmission-Session-Id"] = session_id
            r = client.post(settings.TRANSMISSION_URL, json=payload, auth=transmission_auth())
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
    with engine.begin() as cx:
        cx.execute(text("""
            INSERT INTO history (mam_id, title, author, narrator, dl, torrent_status, torrent_hash, added_at)
            VALUES (:mam_id, :title, :author, :narrator, :dl, :torrent_status, :torrent_hash, :added_at)
        """), {
            "mam_id": mam_id,
            "title": title,
            "author": author,
            "narrator": narrator,
            "dl": dl,
            "torrent_status": "added",
            "torrent_hash": torrent_hash,
            "added_at": datetime.utcnow().strftime("%Y-%m-%d %H:%M:%S"),
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
                args = await transmission_rpc_async(
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
        args = await transmission_rpc_async(
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
            SELECT id, mam_id, title, author, narrator, dl, torrent_hash, added_at, torrent_status
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
@app.get("/transmission/torrents")
async def transmission_torrents():
    async with httpx.AsyncClient(timeout=30) as c:
        args = await transmission_rpc_async(c, "torrent-get", {
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
        return {"items": out}
        
# ---------------------------- Perform Import ----------------------------

from pathlib import Path
import shutil

AUDIO_EXTS = None  # copy everything except .cue (per your request)

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

class ImportBody(BaseModel):
    author: str
    title: str
    hash: str
    history_id: int | None = None

@app.post("/import")
def do_import(body: ImportBody):
    author = sanitize(body.author)
    title = sanitize(body.title)
    h = body.hash

    # Query Transmission for files and download directory.
    with httpx.Client(timeout=30) as c:
        args = transmission_rpc_sync(c, "torrent-get", {
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
        existing_labels = info.get("labels") or []

    # Transmission and this app must share the same static in-container mount.
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

    # --- post-import: remove app labels so the torrent disappears from our list ---
    if h:
        remaining_labels = [
            label for label in existing_labels
            if label != settings.TRANSMISSION_LABEL and not str(label).startswith("mamid=")
        ]
        try:
            with httpx.Client(timeout=15) as c2:
                transmission_rpc_sync(c2, "torrent-set", {
                    "ids": [h],
                    "labels": remaining_labels,
                })
        except Exception:
            # Best effort: don't fail the import if this errors.
            pass

    # --- mark history as imported ---
    with engine.begin() as cx:
        if body.history_id is not None:
            cx.execute(
                text("UPDATE history SET torrent_status='imported', imported_at=:ts WHERE id=:id"),
                {"ts": datetime.utcnow().strftime("%Y-%m-%d %H:%M:%S"), "id": body.history_id},
            )
        else:
            # Fallback: try by torrent hash if we have it
            cx.execute(
                text("UPDATE history SET torrent_status='imported', imported_at=:ts WHERE torrent_hash=:h"),
                {"ts": datetime.utcnow().strftime("%Y-%m-%d %H:%M:%S"), "h": body.hash},
            )

    return {"ok": True, "dest": str(dest_dir)}
