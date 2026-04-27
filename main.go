package main

import (
	"bytes"
	"context"
	"embed"
	"encoding/base64"
	"encoding/json"
	"errors"
	"fmt"
	"html/template"
	"io"
	"io/fs"
	"log"
	"net/http"
	"os"
	"os/exec"
	"path/filepath"
	"regexp"
	"strconv"
	"strings"
	"sync"
	"sync/atomic"
	"syscall"
	"time"
)

const (
	defaultTransmissionURL       = "http://transmission:9091/transmission/rpc"
	defaultMAMBase               = "https://www.myanonamouse.net"
	defaultTransmissionLabel     = "mam-audiofinder"
	defaultAutoImportPollSeconds = 30
	downloadsDir                 = "/downloads"
	libraryDir                   = "/library"
	ebooksDir                    = "/ebooks"
	mediaTypeAudiobook           = "audiobook"
	mediaTypeEbook               = "ebook"
)

//go:embed app/templates/*.html app/static/* app/static/screenshots/*
var embeddedFiles embed.FS

var (
	appVersion  = "unknown"
	templates   *template.Template
	staticFiles fs.FS
	settingsRef atomic.Pointer[AppSettings]
	autoState   autoImportState
	logger      = log.New(os.Stdout, "", log.LstdFlags)
)

var (
	appDataDir    = getenv("APP_DATA_DIR", "/data")
	configPath    = getenv("APP_CONFIG_PATH", filepath.Join(appDataDir, "config.json"))
	historyDBPath = filepath.Join(appDataDir, "history.db")
)

var (
	spaceRe  = regexp.MustCompile(`\s+`)
	formatRe = regexp.MustCompile(`(?i)\b(mp3|m4b|flac|aac|ogg|opus|wav|alac|ape|epub|pdf|mobi|azw3|cbz|cbr)\b`)
)

type AppSettings struct {
	MAMBase               string
	MAMCookie             string
	TransmissionURL       string
	TransmissionUser      string
	TransmissionPass      string
	TransmissionLabel     string
	DownloadsDir          string
	LibraryDir            string
	EbooksDir             string
	UMask                 string
	AutoImportEnabled     bool
	AutoImportPollSeconds int
}

type pageData struct {
	AppVersion        string
	SetupEnabled      bool
	TransmissionURL   string
	TransmissionUser  string
	TransmissionLabel string
	AutoImportEnabled bool
}

type SetupPayload struct {
	MAMCookie         string `json:"mam_cookie"`
	TransmissionURL   string `json:"transmission_url"`
	TransmissionUser  string `json:"transmission_user"`
	TransmissionPass  string `json:"transmission_pass"`
	TransmissionLabel string `json:"transmission_label"`
	AutoImportEnabled bool   `json:"auto_import_enabled"`
}

type SearchRequest struct {
	MediaType string         `json:"media_type"`
	Tor       map[string]any `json:"tor"`
	Perpage   int            `json:"perpage"`
}

type AddBody struct {
	ID        any    `json:"id"`
	Title     string `json:"title"`
	DL        string `json:"dl"`
	Author    string `json:"author"`
	Narrator  string `json:"narrator"`
	MediaType string `json:"media_type"`
}

type ImportBody struct {
	Author    string `json:"author"`
	Title     string `json:"title"`
	Hash      string `json:"hash"`
	HistoryID *int   `json:"history_id"`
	MediaType string `json:"media_type"`
}

type apiError struct {
	Status int
	Detail string
}

func (e apiError) Error() string {
	return e.Detail
}

type historyRow struct {
	ID              int     `json:"id"`
	MamID           string  `json:"mam_id"`
	Title           string  `json:"title"`
	Author          string  `json:"author"`
	Narrator        string  `json:"narrator"`
	MediaType       string  `json:"media_type"`
	DL              string  `json:"dl"`
	TorrentHash     string  `json:"torrent_hash"`
	AddedAt         string  `json:"added_at"`
	ImportedAt      *string `json:"imported_at"`
	TorrentStatus   string  `json:"torrent_status"`
	StatusDetail    *string `json:"status_detail"`
	StatusUpdatedAt string  `json:"status_updated_at"`
}

type historyResponse struct {
	Items []historyRow `json:"items"`
}

type searchResult struct {
	ID           string `json:"id"`
	Title        any    `json:"title"`
	AuthorInfo   string `json:"author_info"`
	NarratorInfo string `json:"narrator_info"`
	Format       string `json:"format"`
	Size         any    `json:"size"`
	Seeders      any    `json:"seeders"`
	Leechers     any    `json:"leechers"`
	Catname      any    `json:"catname"`
	Added        any    `json:"added"`
	DL           any    `json:"dl"`
	MediaType    string `json:"media_type"`
	IsFreeleech  bool   `json:"is_freeleech"`
	IsVIP        bool   `json:"is_vip"`
}

type completedTorrent struct {
	Hash        string `json:"hash"`
	Name        string `json:"name"`
	DownloadDir string `json:"download_dir"`
	Root        string `json:"root"`
	SingleFile  bool   `json:"single_file"`
	Size        any    `json:"size"`
	AddedOn     any    `json:"added_on"`
}

func main() {
	if envVersion := os.Getenv("APP_VERSION"); appVersion == "" || appVersion == "unknown" {
		if envVersion != "" {
			appVersion = envVersion
		}
	}
	if appVersion == "" {
		appVersion = "unknown"
	}

	settingsRef.Store(loadSettings())
	applyUMask(settingsRef.Load().UMask)

	if err := openDB(); err != nil {
		logger.Fatalf("failed to open database: %v", err)
	}

	if err := ensureHistorySchema(); err != nil {
		logger.Fatalf("failed to initialize schema: %v", err)
	}

	if err := initTemplates(); err != nil {
		logger.Fatalf("failed to load templates: %v", err)
	}

	var err error
	staticFiles, err = fs.Sub(embeddedFiles, "app/static")
	if err != nil {
		logger.Fatalf("failed to load static assets: %v", err)
	}

	if err := reconcileAutoImportTask(); err != nil {
		logger.Fatalf("failed to start auto-import: %v", err)
	}
	defer autoState.stop()

	mux := http.NewServeMux()
	mux.Handle("/static/", http.StripPrefix("/static/", http.FileServer(http.FS(staticFiles))))
	mux.HandleFunc("/health", healthHandler)
	mux.HandleFunc("/", homeHandler)
	mux.HandleFunc("/setup", setupPageHandler)
	mux.HandleFunc("/api/setup", apiSetupHandler)
	mux.HandleFunc("/search", searchHandler)
	mux.HandleFunc("/add", addHandler)
	mux.HandleFunc("/history", historyHandler)
	mux.HandleFunc("/history/", deleteHistoryHandler)
	mux.HandleFunc("/transmission/torrents", transmissionTorrentsHandler)
	mux.HandleFunc("/import", importHandler)

	logger.Printf("starting server on :8080")
	if err := http.ListenAndServe(":8080", mux); err != nil && !errors.Is(err, http.ErrServerClosed) {
		logger.Fatalf("server failed: %v", err)
	}
}

func initTemplates() error {
	parsed, err := template.New("base").ParseFS(embeddedFiles,
		"app/templates/base.html",
		"app/templates/index.html",
		"app/templates/setup.html",
	)
	if err != nil {
		return err
	}
	templates = parsed
	return nil
}

func openDB() error {
	if _, err := exec.LookPath("sqlite3"); err != nil {
		return err
	}
	if err := os.MkdirAll(filepath.Dir(historyDBPath), 0o755); err != nil {
		return err
	}
	return nil
}

func ensureHistorySchema() error {
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()

	if err := sqliteExec(ctx, "PRAGMA journal_mode=WAL;"); err != nil {
		return err
	}

	createTable := `
		CREATE TABLE IF NOT EXISTS history (
			id INTEGER PRIMARY KEY,
			mam_id TEXT,
			title TEXT,
			author TEXT,
			narrator TEXT,
			media_type TEXT,
			dl TEXT,
			added_at TEXT DEFAULT (datetime('now')),
			imported_at TEXT,
			torrent_status TEXT,
			torrent_hash TEXT
		);
	`
	if err := sqliteExec(ctx, createTable); err != nil {
		return err
	}

	rows, err := sqliteQueryJSON(ctx, "PRAGMA table_info(history);")
	if err != nil {
		return err
	}
	cols := make(map[string]struct{})
	for _, row := range rows {
		cols[stringFromAny(row["name"])] = struct{}{}
	}
	if err := ensureSQLiteColumn(ctx, cols, "status_detail", "ALTER TABLE history ADD COLUMN status_detail TEXT;"); err != nil {
		return err
	}
	if err := ensureSQLiteColumn(ctx, cols, "status_updated_at", "ALTER TABLE history ADD COLUMN status_updated_at TEXT;"); err != nil {
		return err
	}
	if err := ensureSQLiteColumn(ctx, cols, "media_type", "ALTER TABLE history ADD COLUMN media_type TEXT;"); err != nil {
		return err
	}
	for _, stmt := range []string{
		"UPDATE history SET media_type = 'audiobook' WHERE media_type IS NULL OR trim(media_type) = '';",
		"UPDATE history SET torrent_status = 'added' WHERE torrent_status IS NULL OR trim(torrent_status) = '';",
		"UPDATE history SET status_updated_at = COALESCE(status_updated_at, imported_at, added_at) WHERE status_updated_at IS NULL;",
	} {
		if err := sqliteExec(ctx, stmt); err != nil {
			return err
		}
	}
	return nil
}

func ensureSQLiteColumn(ctx context.Context, cols map[string]struct{}, name string, stmt string) error {
	if _, ok := cols[name]; ok {
		return nil
	}
	return sqliteExec(ctx, stmt)
}

func sqliteExec(ctx context.Context, stmt string) error {
	cmd := exec.CommandContext(ctx, "sqlite3", historyDBPath, stmt)
	output, err := cmd.CombinedOutput()
	if err != nil {
		trimmed := strings.TrimSpace(string(output))
		if trimmed != "" {
			return fmt.Errorf("sqlite3 exec failed: %w: %s", err, trimmed)
		}
		return fmt.Errorf("sqlite3 exec failed: %w", err)
	}
	return nil
}

func sqliteQueryJSON(ctx context.Context, stmt string) ([]map[string]any, error) {
	cmd := exec.CommandContext(ctx, "sqlite3", "-json", "-header", historyDBPath, stmt)
	output, err := cmd.CombinedOutput()
	if err != nil {
		trimmed := strings.TrimSpace(string(output))
		if trimmed != "" {
			return nil, fmt.Errorf("sqlite3 query failed: %w: %s", err, trimmed)
		}
		return nil, fmt.Errorf("sqlite3 query failed: %w", err)
	}
	if len(bytes.TrimSpace(output)) == 0 {
		return []map[string]any{}, nil
	}
	var rows []map[string]any
	if err := json.Unmarshal(output, &rows); err != nil {
		return nil, err
	}
	return rows, nil
}

func sqliteQuote(value string) string {
	return "'" + strings.ReplaceAll(value, "'", "''") + "'"
}

func sqliteValue(value any) string {
	switch v := value.(type) {
	case nil:
		return "NULL"
	case string:
		return sqliteQuote(v)
	case *string:
		if v == nil {
			return "NULL"
		}
		return sqliteQuote(*v)
	case bool:
		if v {
			return "1"
		}
		return "0"
	default:
		return fmt.Sprint(v)
	}
}

func healthHandler(w http.ResponseWriter, r *http.Request) {
	if r.Method != http.MethodGet {
		methodNotAllowed(w)
		return
	}
	writeJSON(w, http.StatusOK, map[string]any{"ok": true, "version": appVersion})
}

func homeHandler(w http.ResponseWriter, r *http.Request) {
	if r.URL.Path != "/" {
		http.NotFound(w, r)
		return
	}
	if r.Method != http.MethodGet {
		methodNotAllowed(w)
		return
	}
	data := basePageData()
	if needsSetup() && !isSetupDisabled() {
		renderTemplate(w, "base", "setup", data)
		return
	}
	data.SetupEnabled = !isSetupDisabled()
	renderTemplate(w, "base", "index", data)
}

func setupPageHandler(w http.ResponseWriter, r *http.Request) {
	if r.Method != http.MethodGet {
		methodNotAllowed(w)
		return
	}
	if isSetupDisabled() {
		writeJSONError(w, apiError{Status: http.StatusNotFound, Detail: "Not found"})
		return
	}
	renderTemplate(w, "base", "setup", setupPageData())
}

func apiSetupHandler(w http.ResponseWriter, r *http.Request) {
	if r.Method != http.MethodPost {
		methodNotAllowed(w)
		return
	}
	if isSetupDisabled() {
		writeJSONError(w, apiError{Status: http.StatusNotFound, Detail: "Not found"})
		return
	}

	var body SetupPayload
	if err := decodeJSON(r, &body); err != nil {
		writeJSONError(w, apiError{Status: http.StatusBadRequest, Detail: "Invalid JSON body"})
		return
	}

	cfg := loadJSONConfig()
	if cfg == nil {
		cfg = map[string]any{}
	}

	if strings.TrimSpace(body.MAMCookie) != "" {
		cfg["MAM_COOKIE"] = strings.TrimSpace(body.MAMCookie)
	}
	if strings.TrimSpace(body.TransmissionURL) != "" {
		cfg["TRANSMISSION_URL"] = strings.TrimSpace(body.TransmissionURL)
	}
	if strings.TrimSpace(body.TransmissionUser) != "" {
		cfg["TRANSMISSION_USER"] = strings.TrimSpace(body.TransmissionUser)
	}
	if body.TransmissionPass != "" {
		cfg["TRANSMISSION_PASS"] = body.TransmissionPass
	}
	if strings.TrimSpace(body.TransmissionLabel) != "" {
		cfg["TRANSMISSION_LABEL"] = strings.TrimSpace(body.TransmissionLabel)
	}
	cfg["AUTO_IMPORT_ENABLED"] = body.AutoImportEnabled

	if err := saveJSONConfig(cfg); err != nil {
		writeJSONError(w, apiError{Status: http.StatusInternalServerError, Detail: fmt.Sprintf("Failed to write config: %v", err)})
		return
	}

	settingsRef.Store(loadSettings())
	if err := reconcileAutoImportTask(); err != nil {
		writeJSONError(w, apiError{Status: http.StatusInternalServerError, Detail: fmt.Sprintf("Failed to update auto-import: %v", err)})
		return
	}

	writeJSON(w, http.StatusOK, map[string]any{"ok": true})
}

func searchHandler(w http.ResponseWriter, r *http.Request) {
	if r.Method != http.MethodPost {
		methodNotAllowed(w)
		return
	}
	settings := currentSettings()
	if settings.MAMCookie == "" {
		writeJSONError(w, apiError{Status: http.StatusInternalServerError, Detail: "MAM_COOKIE not set on server"})
		return
	}

	var body SearchRequest
	if err := decodeJSON(r, &body); err != nil {
		writeJSONError(w, apiError{Status: http.StatusBadRequest, Detail: "Invalid JSON body"})
		return
	}

	mediaType, err := normalizeMediaType(body.MediaType)
	if err != nil {
		writeJSONError(w, apiError{Status: http.StatusBadRequest, Detail: err.Error()})
		return
	}
	tor := map[string]any{}
	for k, v := range body.Tor {
		tor[k] = v
	}
	if _, ok := tor["text"]; !ok {
		tor["text"] = ""
	}
	if mediaType == mediaTypeEbook {
		if _, ok := tor["srchIn"]; !ok {
			tor["srchIn"] = []string{"title", "author"}
		}
	} else {
		if _, ok := tor["srchIn"]; !ok {
			tor["srchIn"] = []string{"title", "author", "narrator"}
		}
	}
	if _, ok := tor["searchType"]; !ok {
		tor["searchType"] = "all"
	}
	tor["sortType"] = "seedersDesc"
	if _, ok := tor["startNumber"]; !ok {
		tor["startNumber"] = "0"
	}
	tor["main_cat"] = []string{mamMainCategory(mediaType)}

	perpage := body.Perpage
	if perpage <= 0 {
		perpage = 25
	}

	payload := map[string]any{
		"tor":     tor,
		"perpage": perpage,
	}

	client := &http.Client{Timeout: 30 * time.Second}
	resp, err := doRequestWithMAM(r.Context(), client, settings.MAMBase+"/tor/js/loadSearchJSONbasic.php", settings.MAMCookie, http.MethodPost, payload, map[string]string{
		"Accept":     "application/json, */*",
		"Origin":     "https://www.myanonamouse.net",
		"Referer":    "https://www.myanonamouse.net/",
		"User-Agent": "Mozilla/5.0",
	}, map[string]string{"dlLink": "1"})
	if err != nil {
		writeJSONError(w, err)
		return
	}
	defer resp.Body.Close()

	if resp.StatusCode != http.StatusOK {
		bodyBytes, _ := io.ReadAll(io.LimitReader(resp.Body, 300))
		writeJSONError(w, apiError{Status: http.StatusBadGateway, Detail: fmt.Sprintf("MAM HTTP %d: %s", resp.StatusCode, string(bodyBytes))})
		return
	}

	bodyBytes, err := io.ReadAll(resp.Body)
	if err != nil {
		writeJSONError(w, apiError{Status: http.StatusBadGateway, Detail: fmt.Sprintf("MAM returned unreadable body: %v", err)})
		return
	}

	var raw struct {
		Data       []map[string]any `json:"data"`
		Total      any              `json:"total"`
		TotalFound any              `json:"total_found"`
	}
	if err := json.Unmarshal(bodyBytes, &raw); err != nil {
		writeJSONError(w, apiError{Status: http.StatusBadGateway, Detail: fmt.Sprintf("MAM returned non-JSON. Body: %s", string(bodyBytes[:min(len(bodyBytes), 300)]))})
		return
	}

	results := make([]searchResult, 0, len(raw.Data))
	for _, item := range raw.Data {
		results = append(results, searchResult{
			ID:           stringFromAny(firstNonEmpty(item["id"], item["tid"])),
			Title:        firstNonEmpty(item["title"], item["name"]),
			AuthorInfo:   flattenValue(item["author_info"]),
			NarratorInfo: flattenValue(item["narrator_info"]),
			Format:       detectFormat(item),
			Size:         item["size"],
			Seeders:      item["seeders"],
			Leechers:     item["leechers"],
			Catname:      item["catname"],
			Added:        item["added"],
			DL:           item["dl"],
			MediaType:    mediaType,
			IsFreeleech:  truthyValue(item["free"]) || truthyValue(item["fl_vip"]),
			IsVIP:        truthyValue(item["vip"]) || truthyValue(item["fl_vip"]),
		})
	}

	writeJSON(w, http.StatusOK, map[string]any{
		"results":     results,
		"total":       raw.Total,
		"total_found": raw.TotalFound,
	})
}

func addHandler(w http.ResponseWriter, r *http.Request) {
	if r.Method != http.MethodPost {
		methodNotAllowed(w)
		return
	}
	settings := currentSettings()
	var body AddBody
	if err := decodeJSON(r, &body); err != nil {
		writeJSONError(w, apiError{Status: http.StatusBadRequest, Detail: "Invalid JSON body"})
		return
	}

	mamID := strings.TrimSpace(stringFromAny(body.ID))
	title := strings.TrimSpace(body.Title)
	author := strings.TrimSpace(body.Author)
	narrator := strings.TrimSpace(body.Narrator)
	mediaType, err := normalizeMediaType(body.MediaType)
	if err != nil {
		writeJSONError(w, apiError{Status: http.StatusBadRequest, Detail: err.Error()})
		return
	}
	dl := strings.TrimSpace(body.DL)

	if mamID == "" && dl == "" {
		writeJSONError(w, apiError{Status: http.StatusBadRequest, Detail: "Missing MAM id and dl; need at least one"})
		return
	}

	client := &http.Client{Timeout: 60 * time.Second}
	directURL := ""
	if dl != "" {
		directURL = strings.TrimRight(settings.MAMBase, "/") + "/tor/download.php/" + dl
	}

	var torrentHash string
	idCandidates := []string{}
	if mamID != "" {
		base := strings.TrimRight(settings.MAMBase, "/")
		idCandidates = []string{
			base + "/tor/download.php?id=" + mamID,
			base + "/tor/download.php?tid=" + mamID,
		}
	}

	if directURL != "" {
		args, err := transmissionRPC(r.Context(), client, "torrent-add", torrentAddArguments(settings, mamID, "filename", directURL))
		if err == nil {
			torrentHash = torrentHashFromAddResult(args)
			if err := insertHistory(mamID, title, author, narrator, mediaType, dl, torrentHash); err != nil {
				writeJSONError(w, apiError{Status: http.StatusInternalServerError, Detail: err.Error()})
				return
			}
			writeJSON(w, http.StatusOK, map[string]any{"ok": true})
			return
		}
		if len(idCandidates) == 0 {
			writeJSONError(w, err)
			return
		}
	}

	mamHeaders := map[string]string{
		"User-Agent": "Mozilla/5.0",
		"Accept":     "application/x-bittorrent, */*",
		"Referer":    "https://www.myanonamouse.net/",
		"Origin":     "https://www.myanonamouse.net",
	}
	var torrentBytes []byte
	for _, url := range idCandidates {
		req, err := http.NewRequestWithContext(r.Context(), http.MethodGet, url, nil)
		if err != nil {
			writeJSONError(w, apiError{Status: http.StatusBadGateway, Detail: fmt.Sprintf("Could not prepare MAM request: %v", err)})
			return
		}
		req.Header.Set("Cookie", settings.MAMCookie)
		for k, v := range mamHeaders {
			req.Header.Set(k, v)
		}
		resp, err := client.Do(req)
		if err != nil {
			continue
		}
		bodyBytes, _ := io.ReadAll(resp.Body)
		_ = resp.Body.Close()
		if resp.StatusCode == http.StatusOK && len(bodyBytes) > 0 {
			torrentBytes = bodyBytes
			break
		}
	}

	if len(torrentBytes) == 0 {
		writeJSONError(w, apiError{Status: http.StatusBadGateway, Detail: "Could not fetch .torrent from MAM (no dl hash and cookie fetch failed)."})
		return
	}

	metainfo := base64.StdEncoding.EncodeToString(torrentBytes)
	args, err := transmissionRPC(r.Context(), client, "torrent-add", torrentAddArguments(settings, mamID, "metainfo", metainfo))
	if err != nil {
		writeJSONError(w, err)
		return
	}
	torrentHash = torrentHashFromAddResult(args)
	if err := insertHistory(mamID, title, author, narrator, mediaType, dl, torrentHash); err != nil {
		writeJSONError(w, apiError{Status: http.StatusInternalServerError, Detail: err.Error()})
		return
	}

	writeJSON(w, http.StatusOK, map[string]any{"ok": true})
}

func historyHandler(w http.ResponseWriter, r *http.Request) {
	if r.Method != http.MethodGet {
		methodNotAllowed(w)
		return
	}
	ctx, cancel := context.WithTimeout(r.Context(), 10*time.Second)
	defer cancel()

	rows, err := sqliteQueryJSON(ctx, `
		SELECT
			id,
			mam_id,
			title,
			author,
			narrator,
			media_type,
			dl,
			torrent_hash,
			added_at,
			imported_at,
			torrent_status,
			status_detail,
			status_updated_at
		FROM history
		ORDER BY id DESC
		LIMIT 200;
	`)
	if err != nil {
		writeJSONError(w, apiError{Status: http.StatusInternalServerError, Detail: err.Error()})
		return
	}
	items := make([]historyRow, 0, 64)
	for _, rowMap := range rows {
		row := historyRow{
			ID:              intFromAny(rowMap["id"]),
			MamID:           stringFromAny(rowMap["mam_id"]),
			Title:           stringFromAny(rowMap["title"]),
			Author:          stringFromAny(rowMap["author"]),
			Narrator:        stringFromAny(rowMap["narrator"]),
			MediaType:       stringFromAny(rowMap["media_type"]),
			DL:              stringFromAny(rowMap["dl"]),
			TorrentHash:     stringFromAny(rowMap["torrent_hash"]),
			AddedAt:         stringFromAny(rowMap["added_at"]),
			TorrentStatus:   stringFromAny(rowMap["torrent_status"]),
			StatusUpdatedAt: stringFromAny(rowMap["status_updated_at"]),
		}
		if s := stringFromAny(rowMap["imported_at"]); s != "" {
			row.ImportedAt = &s
		}
		if s := stringFromAny(rowMap["status_detail"]); s != "" {
			row.StatusDetail = &s
		}
		items = append(items, row)
	}

	writeJSON(w, http.StatusOK, historyResponse{Items: items})
}

func deleteHistoryHandler(w http.ResponseWriter, r *http.Request) {
	if r.Method != http.MethodDelete {
		methodNotAllowed(w)
		return
	}
	const prefix = "/history/"
	if !strings.HasPrefix(r.URL.Path, prefix) {
		http.NotFound(w, r)
		return
	}
	idText := strings.TrimPrefix(r.URL.Path, prefix)
	if idText == "" || strings.Contains(idText, "/") {
		http.NotFound(w, r)
		return
	}
	if _, err := strconv.Atoi(idText); err != nil {
		writeJSONError(w, apiError{Status: http.StatusBadRequest, Detail: "Invalid history id"})
		return
	}
	ctx, cancel := context.WithTimeout(r.Context(), 10*time.Second)
	defer cancel()
	if err := sqliteExec(ctx, "DELETE FROM history WHERE id = "+idText+";"); err != nil {
		writeJSONError(w, apiError{Status: http.StatusInternalServerError, Detail: err.Error()})
		return
	}
	writeJSON(w, http.StatusOK, map[string]any{"ok": true})
}

func transmissionTorrentsHandler(w http.ResponseWriter, r *http.Request) {
	if r.Method != http.MethodGet {
		methodNotAllowed(w)
		return
	}
	items, err := listCompletedTorrents(r.Context())
	if err != nil {
		writeJSONError(w, err)
		return
	}
	writeJSON(w, http.StatusOK, map[string]any{"items": items})
}

func importHandler(w http.ResponseWriter, r *http.Request) {
	if r.Method != http.MethodPost {
		methodNotAllowed(w)
		return
	}
	var body ImportBody
	if err := decodeJSON(r, &body); err != nil {
		writeJSONError(w, apiError{Status: http.StatusBadRequest, Detail: "Invalid JSON body"})
		return
	}

	historyID := body.HistoryID
	if historyID != nil {
		if err := updateHistoryStatus(*historyID, "importing", "", nil); err != nil {
			writeJSONError(w, apiError{Status: http.StatusInternalServerError, Detail: err.Error()})
			return
		}
	}

	mediaType := body.MediaType
	if historyID != nil {
		if stored, err := getHistoryMediaType(*historyID); err == nil && stored != "" {
			mediaType = stored
		}
	}

	dest, err := importTorrentToLibrary(r.Context(), body.Author, body.Title, body.Hash, mediaType)
	if err != nil {
		if historyID != nil {
			_ = markHistoryFailed(historyID, body.Hash, err.Error())
		}
		writeJSONError(w, err)
		return
	}

	if err := markHistoryImported(historyID, body.Hash); err != nil {
		writeJSONError(w, apiError{Status: http.StatusInternalServerError, Detail: err.Error()})
		return
	}

	writeJSON(w, http.StatusOK, map[string]any{"ok": true, "dest": dest})
}

func basePageData() pageData {
	settings := currentSettings()
	return pageData{
		AppVersion:        appVersion,
		SetupEnabled:      !isSetupDisabled(),
		TransmissionURL:   settings.TransmissionURL,
		TransmissionUser:  settings.TransmissionUser,
		TransmissionLabel: settings.TransmissionLabel,
		AutoImportEnabled: settings.AutoImportEnabled,
	}
}

func setupPageData() pageData {
	return basePageData()
}

func renderTemplate(w http.ResponseWriter, baseName, pageName string, data pageData) {
	w.Header().Set("Content-Type", "text/html; charset=utf-8")
	if err := templates.ExecuteTemplate(w, baseName, data); err != nil {
		logger.Printf("template render failed (%s): %v", pageName, err)
		http.Error(w, "Template render failed", http.StatusInternalServerError)
	}
}

func loadJSONConfig() map[string]any {
	data, err := os.ReadFile(configPath)
	if err != nil {
		return map[string]any{}
	}
	var cfg map[string]any
	if err := json.Unmarshal(data, &cfg); err != nil || cfg == nil {
		return map[string]any{}
	}
	return cfg
}

func saveJSONConfig(cfg map[string]any) error {
	dir := filepath.Dir(configPath)
	if dir != "" {
		if err := os.MkdirAll(dir, 0o755); err != nil {
			return err
		}
	}
	data, err := json.MarshalIndent(cfg, "", "  ")
	if err != nil {
		return err
	}
	data = append(data, '\n')
	return os.WriteFile(configPath, data, 0o644)
}

func loadSettings() *AppSettings {
	cfg := loadJSONConfig()
	settings := &AppSettings{
		MAMBase:               strings.TrimRight(configStringOrFallback(cfg, "MAM_BASE", getenv("MAM_BASE", defaultMAMBase)), "/"),
		TransmissionURL:       strings.TrimRight(configStringOrFallback(cfg, "TRANSMISSION_URL", getenv("TRANSMISSION_URL", defaultTransmissionURL)), "/"),
		TransmissionUser:      configStringOrFallback(cfg, "TRANSMISSION_USER", getenv("TRANSMISSION_USER", "")),
		TransmissionPass:      configStringOrFallback(cfg, "TRANSMISSION_PASS", getenv("TRANSMISSION_PASS", "")),
		TransmissionLabel:     configStringOrFallback(cfg, "TRANSMISSION_LABEL", getenv("TRANSMISSION_LABEL", defaultTransmissionLabel)),
		DownloadsDir:          downloadsDir,
		LibraryDir:            libraryDir,
		EbooksDir:             ebooksDir,
		UMask:                 configStringOrFallback(cfg, "UMASK", getenv("UMASK", "")),
		AutoImportPollSeconds: parsePositiveInt(getenv("AUTO_IMPORT_POLL_INTERVAL", ""), defaultAutoImportPollSeconds),
	}

	if raw, ok := cfg["MAM_COOKIE"]; ok {
		settings.MAMCookie = buildMAMCookie(anyToString(raw))
	} else {
		settings.MAMCookie = buildMAMCookie(getenv("MAM_COOKIE", ""))
	}

	if raw, ok := cfg["AUTO_IMPORT_ENABLED"]; ok {
		settings.AutoImportEnabled = truthyValue(raw)
	} else {
		settings.AutoImportEnabled = truthyValue(getenv("AUTO_IMPORT_ENABLED", ""))
	}

	return settings
}

func currentSettings() *AppSettings {
	if s := settingsRef.Load(); s != nil {
		return s
	}
	return loadSettings()
}

func needsSetup() bool {
	return currentSettings().MAMCookie == ""
}

func isSetupDisabled() bool {
	return truthyValue(getenv("DISABLE_SETUP", ""))
}

func applyUMask(value string) {
	if strings.TrimSpace(value) == "" {
		return
	}
	parsed, err := strconv.ParseInt(strings.TrimSpace(value), 8, 32)
	if err != nil {
		return
	}
	syscall.Umask(int(parsed))
}

func buildMAMCookie(raw string) string {
	raw = strings.TrimSpace(raw)
	if raw == "" {
		return ""
	}
	if strings.Contains(raw, "mam_id=") || strings.Contains(raw, "mam_session=") {
		return raw
	}
	if !strings.Contains(raw, "=") && !strings.Contains(raw, ";") {
		return "mam_id=" + raw
	}
	return raw
}

func normalizeMediaType(value string) (string, error) {
	switch strings.ToLower(strings.TrimSpace(value)) {
	case "", mediaTypeAudiobook, "audiobooks", "audio":
		return mediaTypeAudiobook, nil
	case mediaTypeEbook, "ebooks", "e-book", "e-books":
		return mediaTypeEbook, nil
	default:
		return "", errors.New("media_type must be audiobook or ebook")
	}
}

func mamMainCategory(mediaType string) string {
	if mediaType == mediaTypeEbook {
		return "14"
	}
	return "13"
}

func doRequestWithMAM(ctx context.Context, client *http.Client, url string, cookie string, method string, body map[string]any, headers map[string]string, query map[string]string) (*http.Response, error) {
	jsonBody, err := json.Marshal(body)
	if err != nil {
		return nil, apiError{Status: http.StatusInternalServerError, Detail: err.Error()}
	}
	req, err := http.NewRequestWithContext(ctx, method, url, bytes.NewReader(jsonBody))
	if err != nil {
		return nil, apiError{Status: http.StatusBadGateway, Detail: fmt.Sprintf("MAM request failed: %v", err)}
	}
	req.Header.Set("Cookie", cookie)
	req.Header.Set("Content-Type", "application/json")
	for k, v := range headers {
		req.Header.Set(k, v)
	}
	q := req.URL.Query()
	for k, v := range query {
		q.Set(k, v)
	}
	req.URL.RawQuery = q.Encode()
	return client.Do(req)
}

func transmissionAuth(settings *AppSettings) (string, string, bool) {
	if settings.TransmissionUser != "" || settings.TransmissionPass != "" {
		return settings.TransmissionUser, settings.TransmissionPass, true
	}
	return "", "", false
}

func transmissionRPC(ctx context.Context, client *http.Client, method string, arguments map[string]any) (map[string]any, error) {
	settings := currentSettings()
	payload := map[string]any{"method": method, "arguments": arguments}
	body, err := json.Marshal(payload)
	if err != nil {
		return nil, apiError{Status: http.StatusInternalServerError, Detail: err.Error()}
	}

	doReq := func(sessionID string) (*http.Response, error) {
		req, err := http.NewRequestWithContext(ctx, http.MethodPost, settings.TransmissionURL, bytes.NewReader(body))
		if err != nil {
			return nil, err
		}
		req.Header.Set("Content-Type", "application/json")
		if sessionID != "" {
			req.Header.Set("X-Transmission-Session-Id", sessionID)
		}
		if user, pass, ok := transmissionAuth(settings); ok {
			req.SetBasicAuth(user, pass)
		}
		return client.Do(req)
	}

	resp, err := doReq("")
	if err != nil {
		return nil, apiError{Status: http.StatusBadGateway, Detail: fmt.Sprintf("Transmission RPC failed: %v", err)}
	}
	defer resp.Body.Close()

	if resp.StatusCode == http.StatusConflict {
		sessionID := resp.Header.Get("X-Transmission-Session-Id")
		_, _ = io.Copy(io.Discard, resp.Body)
		_ = resp.Body.Close()
		if sessionID != "" {
			resp, err = doReq(sessionID)
			if err != nil {
				return nil, apiError{Status: http.StatusBadGateway, Detail: fmt.Sprintf("Transmission RPC failed: %v", err)}
			}
			defer resp.Body.Close()
		}
	}

	bodyBytes, err := io.ReadAll(resp.Body)
	if err != nil {
		return nil, apiError{Status: http.StatusBadGateway, Detail: fmt.Sprintf("Transmission RPC failed: %v", err)}
	}
	if resp.StatusCode != http.StatusOK {
		return nil, apiError{Status: http.StatusBadGateway, Detail: fmt.Sprintf("Transmission RPC failed: %d %s", resp.StatusCode, string(bodyBytes[:min(len(bodyBytes), 160)]))}
	}

	var parsed struct {
		Result    string         `json:"result"`
		Arguments map[string]any `json:"arguments"`
	}
	if err := json.Unmarshal(bodyBytes, &parsed); err != nil {
		return nil, apiError{Status: http.StatusBadGateway, Detail: fmt.Sprintf("Transmission returned non-JSON: %s", string(bodyBytes[:min(len(bodyBytes), 160)]))}
	}
	if parsed.Result != "success" {
		return nil, apiError{Status: http.StatusBadGateway, Detail: fmt.Sprintf("Transmission %s failed: %s", method, parsed.Result)}
	}
	if parsed.Arguments == nil {
		parsed.Arguments = map[string]any{}
	}
	return parsed.Arguments, nil
}

func transmissionLabels(settings *AppSettings, mamID string) []string {
	labels := []string{}
	if settings.TransmissionLabel != "" {
		labels = append(labels, settings.TransmissionLabel)
	}
	if mamID != "" {
		labels = append(labels, "mamid="+mamID)
	}
	return labels
}

func torrentAddArguments(settings *AppSettings, mamID string, sourceKey string, sourceValue string) map[string]any {
	args := map[string]any{sourceKey: sourceValue}
	if labels := transmissionLabels(settings, mamID); len(labels) > 0 {
		args["labels"] = labels
	}
	return args
}

func torrentHashFromAddResult(args map[string]any) string {
	for _, key := range []string{"torrent-added", "torrent-duplicate"} {
		if v, ok := args[key]; ok {
			if m, ok := v.(map[string]any); ok {
				return stringFromAny(m["hashString"])
			}
		}
	}
	return ""
}

func insertHistory(mamID, title, author, narrator, mediaType, dl, torrentHash string) error {
	mediaType, err := normalizeMediaType(mediaType)
	if err != nil {
		return err
	}
	now := utcNowString()
	stmt := fmt.Sprintf(
		"INSERT INTO history (mam_id, title, author, narrator, media_type, dl, torrent_status, torrent_hash, added_at, status_detail, status_updated_at) VALUES (%s, %s, %s, %s, %s, %s, %s, %s, %s, NULL, %s);",
		sqliteValue(mamID),
		sqliteValue(title),
		sqliteValue(author),
		sqliteValue(narrator),
		sqliteValue(mediaType),
		sqliteValue(dl),
		sqliteValue("added"),
		sqliteValue(torrentHash),
		sqliteValue(now),
		sqliteValue(now),
	)
	return sqliteExec(context.Background(), stmt)
}

func listCompletedTorrents(ctx context.Context) ([]completedTorrent, error) {
	client := &http.Client{Timeout: 30 * time.Second}
	args, err := transmissionRPC(ctx, client, "torrent-get", map[string]any{
		"fields": []string{
			"id",
			"hashString",
			"name",
			"percentDone",
			"downloadDir",
			"totalSize",
			"addedDate",
			"labels",
			"files",
		},
	})
	if err != nil {
		return nil, err
	}
	rawTorrents, _ := args["torrents"].([]any)
	if len(rawTorrents) == 0 {
		return []completedTorrent{}, nil
	}
	settings := currentSettings()
	out := make([]completedTorrent, 0, len(rawTorrents))
	for _, raw := range rawTorrents {
		torrent, ok := raw.(map[string]any)
		if !ok {
			continue
		}
		if settings.TransmissionLabel != "" && !containsString(anyStringSlice(torrent["labels"]), settings.TransmissionLabel) {
			continue
		}
		if toFloat(torrent["percentDone"]) < 1 {
			continue
		}
		hash := stringFromAny(torrent["hashString"])
		if hash == "" {
			continue
		}
		files := anyMapSlice(torrent["files"])
		names := make([]string, 0, len(files))
		roots := map[string]struct{}{}
		for _, file := range files {
			name := strings.TrimLeft(stringFromAny(file["name"]), "/")
			if name == "" {
				continue
			}
			names = append(names, name)
			if idx := strings.Index(name, "/"); idx > 0 {
				roots[name[:idx]] = struct{}{}
			}
		}
		root := stringFromAny(torrent["name"])
		if len(roots) == 1 {
			for k := range roots {
				root = k
			}
		}
		singleFile := len(files) == 1
		if singleFile {
			first := stringFromAny(files[0]["name"])
			singleFile = !strings.Contains(first, "/")
		}
		out = append(out, completedTorrent{
			Hash:        hash,
			Name:        stringFromAny(torrent["name"]),
			DownloadDir: stringFromAny(torrent["downloadDir"]),
			Root:        root,
			SingleFile:  singleFile,
			Size:        torrent["totalSize"],
			AddedOn:     torrent["addedDate"],
		})
	}
	return out, nil
}

func sanitize(name string) string {
	s := strings.TrimSpace(name)
	s = strings.NewReplacer(":", " -", "\\", "﹨", "/", "﹨").Replace(s)
	s = spaceRe.ReplaceAllString(s, " ")
	if len(s) > 200 {
		s = s[:200]
	}
	if s == "" {
		return "Unknown"
	}
	return s
}

func nextAvailable(path string) string {
	if _, err := os.Stat(path); errors.Is(err, os.ErrNotExist) {
		return path
	}
	for i := 2; ; i++ {
		candidate := fmt.Sprintf("%s (%d)", path, i)
		if _, err := os.Stat(candidate); errors.Is(err, os.ErrNotExist) {
			return candidate
		}
	}
}

func copyOne(src, dst string) error {
	if err := os.MkdirAll(filepath.Dir(dst), 0o755); err != nil {
		return err
	}
	in, err := os.Open(src)
	if err != nil {
		return err
	}
	defer in.Close()

	info, err := in.Stat()
	if err != nil {
		return err
	}
	out, err := os.Create(dst)
	if err != nil {
		return err
	}
	if _, err := io.Copy(out, in); err != nil {
		_ = out.Close()
		return err
	}
	if err := out.Close(); err != nil {
		return err
	}
	if err := os.Chmod(dst, info.Mode()); err != nil {
		return err
	}
	_ = os.Chtimes(dst, info.ModTime(), info.ModTime())
	return nil
}

func cleanStatusDetail(detail string) string {
	value := strings.TrimSpace(detail)
	value = spaceRe.ReplaceAllString(value, " ")
	if len(value) > 500 {
		value = value[:500]
	}
	return value
}

func updateHistoryStatus(historyID int, status string, detail string, importedAt *string) error {
	now := utcNowString()
	var importedAtValue any
	if importedAt != nil {
		importedAtValue = *importedAt
	}
	detailValue := "NULL"
	if cleaned := cleanStatusDetail(detail); cleaned != "" {
		detailValue = sqliteValue(cleaned)
	}
	importedValue := "NULL"
	if importedAtValue != nil {
		importedValue = sqliteValue(importedAtValue)
	}
	stmt := fmt.Sprintf(
		"UPDATE history SET torrent_status = %s, status_detail = %s, status_updated_at = %s, imported_at = COALESCE(%s, imported_at) WHERE id = %d;",
		sqliteValue(status),
		detailValue,
		sqliteValue(now),
		importedValue,
		historyID,
	)
	return sqliteExec(context.Background(), stmt)
}

func markHistoryImported(historyID *int, torrentHash string) error {
	now := utcNowString()
	if historyID != nil {
		stmt := fmt.Sprintf(
			"UPDATE history SET torrent_status = 'imported', status_detail = NULL, status_updated_at = %s, imported_at = %s WHERE id = %d;",
			sqliteValue(now),
			sqliteValue(now),
			*historyID,
		)
		return sqliteExec(context.Background(), stmt)
	}
	stmt := fmt.Sprintf(
		"UPDATE history SET torrent_status = 'imported', status_detail = NULL, status_updated_at = %s, imported_at = %s WHERE torrent_hash = %s;",
		sqliteValue(now),
		sqliteValue(now),
		sqliteValue(torrentHash),
	)
	return sqliteExec(context.Background(), stmt)
}

func markHistoryFailed(historyID *int, torrentHash string, detail string) error {
	now := utcNowString()
	if historyID != nil {
		detailValue := "NULL"
		if cleaned := cleanStatusDetail(detail); cleaned != "" {
			detailValue = sqliteValue(cleaned)
		}
		stmt := fmt.Sprintf(
			"UPDATE history SET torrent_status = 'import_failed', status_detail = %s, status_updated_at = %s WHERE id = %d;",
			detailValue,
			sqliteValue(now),
			*historyID,
		)
		return sqliteExec(context.Background(), stmt)
	}
	detailValue := "NULL"
	if cleaned := cleanStatusDetail(detail); cleaned != "" {
		detailValue = sqliteValue(cleaned)
	}
	stmt := fmt.Sprintf(
		"UPDATE history SET torrent_status = 'import_failed', status_detail = %s, status_updated_at = %s WHERE torrent_hash = %s;",
		detailValue,
		sqliteValue(now),
		sqliteValue(torrentHash),
	)
	return sqliteExec(context.Background(), stmt)
}

func getHistoryMediaType(historyID int) (string, error) {
	rows, err := sqliteQueryJSON(context.Background(), fmt.Sprintf("SELECT media_type FROM history WHERE id = %d LIMIT 1;", historyID))
	if err != nil {
		return "", err
	}
	if len(rows) == 0 {
		return "", nil
	}
	mediaType, err := normalizeMediaType(stringFromAny(rows[0]["media_type"]))
	if err != nil {
		return "", nil
	}
	return mediaType, nil
}

func getAutoImportCandidates(completedHashes map[string]struct{}) ([]map[string]any, error) {
	if len(completedHashes) == 0 {
		return []map[string]any{}, nil
	}
	rows, err := sqliteQueryJSON(context.Background(), `
		SELECT id, title, author, torrent_hash, torrent_status, media_type
		FROM history
		WHERE
			torrent_hash IS NOT NULL
			AND trim(torrent_hash) != ''
			AND (
				torrent_status IS NULL
				OR torrent_status NOT IN ('imported', 'import_failed', 'importing')
			)
		ORDER BY id ASC;
	`)
	if err != nil {
		return nil, err
	}

	out := []map[string]any{}
	seen := map[string]struct{}{}
	for _, row := range rows {
		id := intFromAny(row["id"])
		title := stringFromAny(row["title"])
		author := stringFromAny(row["author"])
		torrentHash := strings.TrimSpace(stringFromAny(row["torrent_hash"]))
		if torrentHash == "" {
			continue
		}
		if _, ok := completedHashes[torrentHash]; !ok {
			continue
		}
		if _, ok := seen[torrentHash]; ok {
			continue
		}
		seen[torrentHash] = struct{}{}
		out = append(out, map[string]any{
			"id":             id,
			"title":          title,
			"author":         author,
			"torrent_hash":   torrentHash,
			"torrent_status": stringFromAny(row["torrent_status"]),
			"media_type":     stringFromAny(row["media_type"]),
		})
	}
	return out, nil
}

func validateDownloadPath(p string) (string, error) {
	p = strings.TrimSpace(p)
	if p == "" {
		return p, nil
	}
	base := strings.TrimRight(downloadsDir, "/")
	if p == base || strings.HasPrefix(p, base+"/") {
		return p, nil
	}
	return "", apiError{
		Status: http.StatusBadRequest,
		Detail: fmt.Sprintf(
			"Transmission reports downloadDir '%s', but this app expects completed downloads under %s. Mount the same downloads directory at %s in both containers.",
			p,
			downloadsDir,
			downloadsDir,
		),
	}
}

func isTransientAutoImportError(err error) bool {
	var apiErr apiError
	if errors.As(err, &apiErr) {
		return apiErr.Status == http.StatusBadGateway && strings.HasPrefix(apiErr.Detail, "Transmission")
	}
	return false
}

func importTorrentToLibrary(ctx context.Context, author string, title string, torrentHash string, mediaType string) (string, error) {
	mediaType, err := normalizeMediaType(mediaType)
	if err != nil {
		return "", apiError{Status: http.StatusBadRequest, Detail: err.Error()}
	}
	author = sanitize(author)
	title = sanitize(title)

	client := &http.Client{Timeout: 30 * time.Second}
	args, err := transmissionRPC(ctx, client, "torrent-get", map[string]any{
		"ids":    []string{torrentHash},
		"fields": []string{"id", "hashString", "name", "downloadDir", "labels", "files"},
	})
	if err != nil {
		return "", err
	}

	torrents := anyMapSlice(args["torrents"])
	if len(torrents) == 0 {
		return "", apiError{Status: http.StatusNotFound, Detail: "No files found for torrent"}
	}
	info := torrents[0]
	files := anyMapSlice(info["files"])
	if len(files) == 0 {
		return "", apiError{Status: http.StatusNotFound, Detail: "No files found for torrent"}
	}

	downloadDir, err := validateDownloadPath(strings.TrimRight(stringFromAny(info["downloadDir"]), "/"))
	if err != nil {
		return "", err
	}
	if downloadDir == "" {
		return "", apiError{Status: http.StatusNotFound, Detail: "Torrent download directory not found"}
	}

	sourceDir := filepath.Clean(downloadDir)
	baseDir := libraryDir
	if mediaType == mediaTypeEbook {
		baseDir = ebooksDir
	}
	authorDir := filepath.Join(baseDir, author)
	if err := os.MkdirAll(authorDir, 0o755); err != nil {
		return "", err
	}
	destDir := nextAvailable(filepath.Join(authorDir, title))

	names := make([]string, 0, len(files))
	for _, file := range files {
		name := strings.TrimLeft(stringFromAny(file["name"]), "/")
		if name == "" {
			continue
		}
		names = append(names, name)
	}

	var commonRoot string
	if len(names) > 0 {
		commonRoot = topLevelCommonRoot(names)
	}

	copied := 0
	if len(names) == 1 {
		src := filepath.Join(sourceDir, names[0])
		if strings.EqualFold(filepath.Ext(src), ".cue") {
			return "", apiError{Status: http.StatusBadRequest, Detail: "Only .cue file found; nothing to import"}
		}
		if err := copyOne(src, filepath.Join(destDir, filepath.Base(src))); err != nil {
			return "", err
		}
		copied++
	} else {
		for _, name := range names {
			src := filepath.Join(sourceDir, name)
			if strings.EqualFold(filepath.Ext(src), ".cue") {
				continue
			}
			relName := name
			if commonRoot != "" && strings.HasPrefix(name, commonRoot+"/") {
				relName = strings.TrimPrefix(name, commonRoot+"/")
			}
			if relName == "" {
				continue
			}
			if err := copyOne(src, filepath.Join(destDir, relName)); err != nil {
				return "", err
			}
			copied++
		}
	}

	if copied == 0 {
		return "", apiError{Status: http.StatusBadRequest, Detail: "No importable files found"}
	}

	return destDir, nil
}

func topLevelCommonRoot(names []string) string {
	roots := map[string]struct{}{}
	for _, name := range names {
		if idx := strings.Index(name, "/"); idx > 0 {
			roots[name[:idx]] = struct{}{}
		}
	}
	if len(roots) != 1 {
		return ""
	}
	for root := range roots {
		for _, name := range names {
			if name != root && !strings.HasPrefix(name, root+"/") {
				return ""
			}
		}
		return root
	}
	return ""
}

func autoImportCycle(ctx context.Context) {
	completed, err := listCompletedTorrents(ctx)
	if err != nil {
		logger.Printf("auto-import cycle skipped: %v", err)
		return
	}
	completedHashes := map[string]struct{}{}
	for _, item := range completed {
		if item.Hash != "" {
			completedHashes[item.Hash] = struct{}{}
		}
	}
	rows, err := getAutoImportCandidates(completedHashes)
	if err != nil {
		logger.Printf("auto-import candidate load failed: %v", err)
		return
	}
	for _, row := range rows {
		historyID := intFromAny(row["id"])
		torrentHash := strings.TrimSpace(stringFromAny(row["torrent_hash"]))
		author := strings.TrimSpace(stringFromAny(row["author"]))
		title := strings.TrimSpace(stringFromAny(row["title"]))
		mediaType, err := normalizeMediaType(stringFromAny(row["media_type"]))
		if err != nil {
			_ = markHistoryFailed(&historyID, torrentHash, err.Error())
			continue
		}
		if author == "" || title == "" {
			_ = markHistoryFailed(&historyID, torrentHash, "History row is missing author/title; use manual import.")
			continue
		}
		if err := updateHistoryStatus(historyID, "importing", "", nil); err != nil {
			logger.Printf("auto-import status update failed for history row %d: %v", historyID, err)
			continue
		}
		dest, err := importTorrentToLibrary(ctx, author, title, torrentHash, mediaType)
		if err != nil {
			if isTransientAutoImportError(err) {
				_ = updateHistoryStatus(historyID, "added", "", nil)
				logger.Printf("auto-import skipped for history row %d: %v", historyID, err)
			} else {
				_ = markHistoryFailed(&historyID, torrentHash, err.Error())
				logger.Printf("auto-import failed for history row %d: %v", historyID, err)
			}
			continue
		}
		_ = dest
		_ = markHistoryImported(&historyID, torrentHash)
		logger.Printf("auto-imported history row %d", historyID)
	}
}

func autoImportLoop(ctx context.Context) {
	logger.Printf("auto-import poller started with %ss interval", strconv.Itoa(currentSettings().AutoImportPollSeconds))
	defer logger.Printf("auto-import poller stopped")
	for {
		autoImportCycle(ctx)
		timer := time.NewTimer(time.Duration(currentSettings().AutoImportPollSeconds) * time.Second)
		select {
		case <-ctx.Done():
			timer.Stop()
			return
		case <-timer.C:
		}
	}
}

type autoImportState struct {
	mu     sync.Mutex
	cancel context.CancelFunc
	wg     sync.WaitGroup
}

func (s *autoImportState) start() {
	s.mu.Lock()
	if s.cancel != nil {
		s.mu.Unlock()
		return
	}
	ctx, cancel := context.WithCancel(context.Background())
	s.cancel = cancel
	s.wg.Add(1)
	s.mu.Unlock()

	go func() {
		defer s.wg.Done()
		autoImportLoop(ctx)
	}()
}

func (s *autoImportState) stop() {
	s.mu.Lock()
	cancel := s.cancel
	s.cancel = nil
	s.mu.Unlock()
	if cancel != nil {
		cancel()
		s.wg.Wait()
	}
}

func reconcileAutoImportTask() error {
	autoState.stop()
	if currentSettings().AutoImportEnabled {
		autoState.start()
	}
	return nil
}

func writeJSON(w http.ResponseWriter, status int, value any) {
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(status)
	_ = json.NewEncoder(w).Encode(value)
}

func writeJSONError(w http.ResponseWriter, err error) {
	var apiErr apiError
	if errors.As(err, &apiErr) {
		writeJSON(w, apiErr.Status, map[string]any{"detail": apiErr.Detail})
		return
	}
	writeJSON(w, http.StatusInternalServerError, map[string]any{"detail": err.Error()})
}

func decodeJSON(r *http.Request, dst any) error {
	dec := json.NewDecoder(r.Body)
	return dec.Decode(dst)
}

func methodNotAllowed(w http.ResponseWriter) {
	w.Header().Set("Allow", "GET, POST, DELETE")
	writeJSON(w, http.StatusMethodNotAllowed, map[string]any{"detail": "Method not allowed"})
}

func configStringOrFallback(cfg map[string]any, key, fallback string) string {
	if cfg == nil {
		return fallback
	}
	raw, ok := cfg[key]
	if !ok || raw == nil {
		return fallback
	}
	switch v := raw.(type) {
	case string:
		if strings.TrimSpace(v) == "" {
			return fallback
		}
		return v
	default:
		s := fmt.Sprint(v)
		if strings.TrimSpace(s) == "" {
			return fallback
		}
		return s
	}
}

func truthyValue(value any) bool {
	switch v := value.(type) {
	case bool:
		return v
	case string:
		switch strings.ToLower(strings.TrimSpace(v)) {
		case "1", "true", "yes", "on":
			return true
		default:
			return false
		}
	case nil:
		return false
	default:
		return truthyValue(fmt.Sprint(v))
	}
}

func parsePositiveInt(value string, fallback int) int {
	n, err := strconv.Atoi(strings.TrimSpace(value))
	if err != nil || n <= 0 {
		return fallback
	}
	return n
}

func getenv(key, fallback string) string {
	if value := os.Getenv(key); value != "" {
		return value
	}
	return fallback
}

func anyToString(value any) string {
	switch v := value.(type) {
	case nil:
		return ""
	case string:
		return v
	case fmt.Stringer:
		return v.String()
	default:
		return fmt.Sprint(v)
	}
}

func stringFromAny(value any) string {
	return strings.TrimSpace(anyToString(value))
}

func intFromAny(value any) int {
	switch v := value.(type) {
	case int:
		return v
	case int64:
		return int(v)
	case float64:
		return int(v)
	case json.Number:
		n, _ := v.Int64()
		return int(n)
	default:
		n, _ := strconv.Atoi(strings.TrimSpace(anyToString(v)))
		return n
	}
}

func toFloat(value any) float64 {
	switch v := value.(type) {
	case float64:
		return v
	case float32:
		return float64(v)
	case int:
		return float64(v)
	case int64:
		return float64(v)
	case json.Number:
		n, _ := v.Float64()
		return n
	default:
		f, _ := strconv.ParseFloat(strings.TrimSpace(anyToString(v)), 64)
		return f
	}
}

func anyStringSlice(value any) []string {
	arr, ok := value.([]any)
	if !ok {
		if ss, ok := value.([]string); ok {
			return ss
		}
		return nil
	}
	out := make([]string, 0, len(arr))
	for _, item := range arr {
		out = append(out, stringFromAny(item))
	}
	return out
}

func anyMapSlice(value any) []map[string]any {
	arr, ok := value.([]any)
	if !ok {
		if arrMap, ok := value.([]map[string]any); ok {
			return arrMap
		}
		return nil
	}
	out := make([]map[string]any, 0, len(arr))
	for _, item := range arr {
		if m, ok := item.(map[string]any); ok {
			out = append(out, m)
		}
	}
	return out
}

func containsString(values []string, target string) bool {
	for _, value := range values {
		if value == target {
			return true
		}
	}
	return false
}

func firstNonEmpty(values ...any) any {
	for _, value := range values {
		if value == nil {
			continue
		}
		if strings.TrimSpace(anyToString(value)) != "" {
			return value
		}
	}
	return nil
}

func flattenValue(value any) string {
	switch v := value.(type) {
	case map[string]any:
		items := make([]string, 0, len(v))
		for _, item := range v {
			items = append(items, anyToString(item))
		}
		return strings.Join(items, ", ")
	case []any:
		items := make([]string, 0, len(v))
		for _, item := range v {
			items = append(items, anyToString(item))
		}
		return strings.Join(items, ", ")
	case string:
		s := strings.TrimSpace(v)
		if strings.HasPrefix(s, "{") || strings.HasPrefix(s, "[") {
			var decoded any
			if err := json.Unmarshal([]byte(s), &decoded); err == nil {
				return flattenValue(decoded)
			}
		}
		s = strings.Trim(s, "{}")
		parts := strings.Split(s, ",")
		out := make([]string, 0, len(parts))
		for _, chunk := range parts {
			segment := chunk
			if idx := strings.Index(segment, ":"); idx >= 0 {
				segment = segment[idx+1:]
			}
			segment = strings.Trim(segment, `"' `)
			if segment != "" {
				out = append(out, segment)
			}
		}
		return strings.Join(out, ", ")
	default:
		return anyToString(v)
	}
}

func detectFormat(item map[string]any) string {
	for _, key := range []string{"format", "filetype", "container", "encoding", "format_name"} {
		if s := stringFromAny(item[key]); s != "" {
			return s
		}
	}
	name := strings.TrimSpace(anyToString(firstNonEmpty(item["title"], item["name"])))
	matches := formatRe.FindAllString(name, -1)
	if len(matches) > 0 {
		seen := map[string]struct{}{}
		uniq := make([]string, 0, len(matches))
		for _, match := range matches {
			match = strings.ToUpper(match)
			if _, ok := seen[match]; ok {
				continue
			}
			seen[match] = struct{}{}
			uniq = append(uniq, match)
		}
		return strings.Join(uniq, "/")
	}
	return ""
}

func utcNowString() string {
	return time.Now().UTC().Format("2006-01-02 15:04:05")
}

func min(a, b int) int {
	if a < b {
		return a
	}
	return b
}
