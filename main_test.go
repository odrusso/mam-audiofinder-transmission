package main

import (
	"bytes"
	"context"
	"encoding/base64"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func TestBuildMAMCookie(t *testing.T) {
	t.Parallel()

	tests := []struct {
		name string
		in   string
		want string
	}{
		{name: "empty", in: "", want: ""},
		{name: "token", in: "abc123", want: "mam_id=abc123"},
		{name: "header", in: "mam_id=abc123; foo=bar", want: "mam_id=abc123; foo=bar"},
		{name: "session", in: "mam_session=abc123", want: "mam_session=abc123"},
	}

	for _, tt := range tests {
		tt := tt
		t.Run(tt.name, func(t *testing.T) {
			t.Parallel()
			if got := buildMAMCookie(tt.in); got != tt.want {
				t.Fatalf("buildMAMCookie(%q) = %q, want %q", tt.in, got, tt.want)
			}
		})
	}
}

func TestNormalizeMediaType(t *testing.T) {
	t.Parallel()

	tests := []struct {
		name    string
		in      string
		want    string
		wantErr bool
	}{
		{name: "default", in: "", want: mediaTypeAudiobook},
		{name: "audio", in: "audio", want: mediaTypeAudiobook},
		{name: "ebook", in: "ebooks", want: mediaTypeEbook},
		{name: "invalid", in: "vinyl", wantErr: true},
	}

	for _, tt := range tests {
		tt := tt
		t.Run(tt.name, func(t *testing.T) {
			t.Parallel()
			got, err := normalizeMediaType(tt.in)
			if tt.wantErr {
				if err == nil {
					t.Fatalf("normalizeMediaType(%q) = %q, want error", tt.in, got)
				}
				return
			}
			if err != nil {
				t.Fatalf("normalizeMediaType(%q) unexpected error: %v", tt.in, err)
			}
			if got != tt.want {
				t.Fatalf("normalizeMediaType(%q) = %q, want %q", tt.in, got, tt.want)
			}
		})
	}
}

func TestSanitize(t *testing.T) {
	t.Parallel()

	got := sanitize("  A/B: C\\D   ")
	want := "A﹨B - C﹨D"
	if got != want {
		t.Fatalf("sanitize() = %q, want %q", got, want)
	}
}

func TestTopLevelCommonRoot(t *testing.T) {
	t.Parallel()

	if got := topLevelCommonRoot([]string{"Book/track1.mp3", "Book/track2.mp3"}); got != "Book" {
		t.Fatalf("topLevelCommonRoot() = %q, want %q", got, "Book")
	}
	if got := topLevelCommonRoot([]string{"a/one", "b/two"}); got != "" {
		t.Fatalf("topLevelCommonRoot() = %q, want empty", got)
	}
}

func TestDetectFormat(t *testing.T) {
	t.Parallel()

	got := detectFormat(map[string]any{"title": "Example Audio MP3 Pack"})
	if got != "MP3" {
		t.Fatalf("detectFormat() = %q, want %q", got, "MP3")
	}
}

func TestFlattenValue(t *testing.T) {
	t.Parallel()

	got := flattenValue(map[string]any{"1": "Alice", "2": "Bob"})
	if got != "Alice, Bob" && got != "Bob, Alice" {
		t.Fatalf("flattenValue(map) = %q, want joined values", got)
	}

	if got := flattenValue(`{"1":"Alice","2":"Bob"}`); got != "Alice, Bob" && got != "Bob, Alice" {
		t.Fatalf("flattenValue(json string) = %q, want joined values", got)
	}
}

func TestLoadSettingsConfigOverridesEnv(t *testing.T) {
	setupIsolatedState(t)

	t.Setenv("MAM_BASE", "https://env.example")
	t.Setenv("MAM_COOKIE", "env-cookie")
	t.Setenv("TRANSMISSION_URL", "http://env-transmission/rpc")
	t.Setenv("TRANSMISSION_USER", "env-user")
	t.Setenv("TRANSMISSION_PASS", "env-pass")
	t.Setenv("TRANSMISSION_LABEL", "env-label")
	t.Setenv("AUTO_IMPORT_ENABLED", "")

	err := saveJSONConfig(map[string]any{
		"MAM_BASE":            "https://config.example/",
		"MAM_COOKIE":          "config-cookie",
		"TRANSMISSION_URL":    "http://config-transmission/rpc/",
		"TRANSMISSION_USER":   "config-user",
		"TRANSMISSION_PASS":   "config-pass",
		"TRANSMISSION_LABEL":  "config-label",
		"AUTO_IMPORT_ENABLED": true,
	})
	if err != nil {
		t.Fatalf("saveJSONConfig() error = %v", err)
	}

	settings := loadSettings()
	if settings.MAMBase != "https://config.example" {
		t.Fatalf("MAMBase = %q, want config value without trailing slash", settings.MAMBase)
	}
	if settings.MAMCookie != "mam_id=config-cookie" {
		t.Fatalf("MAMCookie = %q, want normalized config cookie", settings.MAMCookie)
	}
	if settings.TransmissionURL != "http://config-transmission/rpc" {
		t.Fatalf("TransmissionURL = %q, want config value without trailing slash", settings.TransmissionURL)
	}
	if settings.TransmissionUser != "config-user" || settings.TransmissionPass != "config-pass" || settings.TransmissionLabel != "config-label" {
		t.Fatalf("Transmission settings did not use config overrides: %+v", settings)
	}
	if !settings.AutoImportEnabled {
		t.Fatalf("AutoImportEnabled = false, want config override true")
	}
}

func TestSetupWritesConfigAndReloadsSettings(t *testing.T) {
	setupIsolatedState(t)

	req := jsonRequest(t, http.MethodPost, "/api/setup", map[string]any{
		"mam_cookie":          "setup-cookie",
		"transmission_url":    "http://transmission.example/rpc",
		"transmission_user":   "setup-user",
		"transmission_pass":   "setup-pass",
		"transmission_label":  "setup-label",
		"auto_import_enabled": false,
	})
	rr := httptest.NewRecorder()
	apiSetupHandler(rr, req)

	assertStatus(t, rr, http.StatusOK)

	cfg := loadJSONConfig()
	if cfg["MAM_COOKIE"] != "setup-cookie" {
		t.Fatalf("saved MAM_COOKIE = %v, want setup-cookie", cfg["MAM_COOKIE"])
	}
	if cfg["TRANSMISSION_URL"] != "http://transmission.example/rpc" {
		t.Fatalf("saved TRANSMISSION_URL = %v", cfg["TRANSMISSION_URL"])
	}

	settings := currentSettings()
	if settings.MAMCookie != "mam_id=setup-cookie" {
		t.Fatalf("reloaded MAMCookie = %q, want normalized setup cookie", settings.MAMCookie)
	}
	if settings.TransmissionURL != "http://transmission.example/rpc" || settings.TransmissionLabel != "setup-label" {
		t.Fatalf("settings were not reloaded from setup config: %+v", settings)
	}

	if _, err := os.Stat(configPath); err != nil {
		t.Fatalf("setup config file was not written: %v", err)
	}
}

func TestSetupDisabledReturns404(t *testing.T) {
	setupIsolatedState(t)
	t.Setenv("DISABLE_SETUP", "true")

	for _, tt := range []struct {
		name    string
		handler http.HandlerFunc
		req     *http.Request
	}{
		{name: "page", handler: setupPageHandler, req: httptest.NewRequest(http.MethodGet, "/setup", nil)},
		{name: "api", handler: apiSetupHandler, req: jsonRequest(t, http.MethodPost, "/api/setup", map[string]any{})},
	} {
		t.Run(tt.name, func(t *testing.T) {
			rr := httptest.NewRecorder()
			tt.handler(rr, tt.req)
			assertStatus(t, rr, http.StatusNotFound)
		})
	}
}

func TestHistoryStorageRoundTrip(t *testing.T) {
	setupIsolatedState(t)

	if err := insertHistory("101", "Title", "Author", "Narrator", mediaTypeEbook, "dl-token", "HASH101"); err != nil {
		t.Fatalf("insertHistory() error = %v", err)
	}

	rows, err := sqliteQueryJSON(context.Background(), "SELECT id, mam_id, title, media_type, torrent_status, torrent_hash FROM history;")
	if err != nil {
		t.Fatalf("sqliteQueryJSON() error = %v", err)
	}
	if len(rows) != 1 {
		t.Fatalf("history row count = %d, want 1", len(rows))
	}
	id := intFromAny(rows[0]["id"])
	if rows[0]["mam_id"] != "101" || rows[0]["media_type"] != mediaTypeEbook || rows[0]["torrent_status"] != "added" || rows[0]["torrent_hash"] != "HASH101" {
		t.Fatalf("unexpected inserted history row: %#v", rows[0])
	}

	if err := updateHistoryStatus(id, "importing", " working\nnow ", nil); err != nil {
		t.Fatalf("updateHistoryStatus() error = %v", err)
	}
	if got, err := getHistoryMediaType(id); err != nil || got != mediaTypeEbook {
		t.Fatalf("getHistoryMediaType() = %q, %v; want %q, nil", got, err, mediaTypeEbook)
	}
	if err := markHistoryImported(&id, ""); err != nil {
		t.Fatalf("markHistoryImported() error = %v", err)
	}

	rows, err = sqliteQueryJSON(context.Background(), "SELECT torrent_status, status_detail, imported_at FROM history WHERE id = 1;")
	if err != nil {
		t.Fatalf("sqliteQueryJSON() error = %v", err)
	}
	if rows[0]["torrent_status"] != "imported" || stringFromAny(rows[0]["status_detail"]) != "" || stringFromAny(rows[0]["imported_at"]) == "" {
		t.Fatalf("unexpected imported history row: %#v", rows[0])
	}
}

func TestSearchHandlerBuildsMediaPayloads(t *testing.T) {
	for _, tt := range []struct {
		name       string
		mediaType  string
		mainCat    string
		wantSrchIn []string
	}{
		{name: "audiobook", mediaType: mediaTypeAudiobook, mainCat: "13", wantSrchIn: []string{"title", "author", "narrator"}},
		{name: "ebook", mediaType: mediaTypeEbook, mainCat: "14", wantSrchIn: []string{"title", "author"}},
	} {
		t.Run(tt.name, func(t *testing.T) {
			setupIsolatedState(t)

			var gotPayload map[string]any
			mam := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
				if r.Method != http.MethodPost {
					t.Errorf("MAM method = %s, want POST", r.Method)
				}
				if r.URL.Path != "/tor/js/loadSearchJSONbasic.php" {
					t.Errorf("MAM path = %s", r.URL.Path)
				}
				if r.URL.Query().Get("dlLink") != "1" {
					t.Errorf("dlLink query = %q, want 1", r.URL.Query().Get("dlLink"))
				}
				if r.Header.Get("Cookie") != "mam_id=test-cookie" {
					t.Errorf("MAM Cookie = %q", r.Header.Get("Cookie"))
				}
				if err := json.NewDecoder(r.Body).Decode(&gotPayload); err != nil {
					t.Errorf("decode MAM payload: %v", err)
				}
				writeJSON(w, http.StatusOK, map[string]any{
					"data": []map[string]any{
						{
							"id":            "42",
							"title":         "Example MP3",
							"author_info":   map[string]any{"1": "Author"},
							"narrator_info": map[string]any{"1": "Narrator"},
							"catname":       tt.mediaType,
							"dl":            "download-token",
							"seeders":       5,
						},
					},
					"total":       1,
					"total_found": 1,
				})
			}))
			defer mam.Close()
			settingsRef.Store(testSettings(mam.URL, "http://transmission.invalid/rpc"))

			rr := httptest.NewRecorder()
			searchHandler(rr, jsonRequest(t, http.MethodPost, "/search", map[string]any{
				"media_type": tt.mediaType,
				"tor":        map[string]any{"text": "example"},
				"perpage":    1,
			}))
			assertStatus(t, rr, http.StatusOK)

			tor, ok := gotPayload["tor"].(map[string]any)
			if !ok {
				t.Fatalf("MAM tor payload = %#v, want object", gotPayload["tor"])
			}
			if got := anyStringSlice(tor["main_cat"]); len(got) != 1 || got[0] != tt.mainCat {
				t.Fatalf("main_cat = %#v, want [%s]", got, tt.mainCat)
			}
			if got := anyStringSlice(tor["srchIn"]); !sameStrings(got, tt.wantSrchIn) {
				t.Fatalf("srchIn = %#v, want %#v", got, tt.wantSrchIn)
			}
			if got := stringFromAny(tor["sortType"]); got != "seedersDesc" {
				t.Fatalf("sortType = %q, want seedersDesc", got)
			}
			if got := toFloat(gotPayload["perpage"]); got != 1 {
				t.Fatalf("perpage = %v, want 1", got)
			}

			var out struct {
				Results []searchResult `json:"results"`
			}
			decodeRecorder(t, rr, &out)
			if len(out.Results) != 1 || out.Results[0].ID != "42" || out.Results[0].MediaType != tt.mediaType {
				t.Fatalf("unexpected search response: %#v", out.Results)
			}
		})
	}
}

func TestSearchHandlerMAMErrorsMapToBadGateway(t *testing.T) {
	for _, tt := range []struct {
		name   string
		status int
		body   string
	}{
		{name: "http error", status: http.StatusServiceUnavailable, body: "MAM down"},
		{name: "invalid json", status: http.StatusOK, body: "not-json"},
	} {
		t.Run(tt.name, func(t *testing.T) {
			setupIsolatedState(t)
			mam := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
				w.WriteHeader(tt.status)
				_, _ = w.Write([]byte(tt.body))
			}))
			defer mam.Close()
			settingsRef.Store(testSettings(mam.URL, "http://transmission.invalid/rpc"))

			rr := httptest.NewRecorder()
			searchHandler(rr, jsonRequest(t, http.MethodPost, "/search", map[string]any{
				"media_type": mediaTypeAudiobook,
				"tor":        map[string]any{"text": "example"},
			}))

			assertStatus(t, rr, http.StatusBadGateway)
		})
	}
}

func TestAddHandlerDirectDLUsesTransmissionFilenameAndHistory(t *testing.T) {
	setupIsolatedState(t)

	var gotMethod string
	var gotArgs map[string]any
	transmission := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		req := decodeTransmissionRequest(t, r)
		gotMethod = req.Method
		gotArgs = req.Arguments
		writeTransmissionSuccess(t, w, map[string]any{
			"torrent-added": map[string]any{"hashString": "HASH1"},
		})
	}))
	defer transmission.Close()
	settingsRef.Store(testSettings("https://mam.example", transmission.URL))

	rr := httptest.NewRecorder()
	addHandler(rr, jsonRequest(t, http.MethodPost, "/add", map[string]any{
		"id":         "123",
		"title":      "Direct Title",
		"author":     "Author",
		"narrator":   "Narrator",
		"media_type": mediaTypeAudiobook,
		"dl":         "direct-token",
	}))
	assertStatus(t, rr, http.StatusOK)

	if gotMethod != "torrent-add" {
		t.Fatalf("Transmission method = %q, want torrent-add", gotMethod)
	}
	if got := stringFromAny(gotArgs["filename"]); got != "https://mam.example/tor/download.php/direct-token" {
		t.Fatalf("filename = %q", got)
	}
	if labels := anyStringSlice(gotArgs["labels"]); !sameStrings(labels, []string{"test-label", "mamid=123"}) {
		t.Fatalf("labels = %#v", labels)
	}

	rows, err := sqliteQueryJSON(context.Background(), "SELECT mam_id, title, media_type, dl, torrent_status, torrent_hash FROM history;")
	if err != nil {
		t.Fatalf("sqliteQueryJSON() error = %v", err)
	}
	if len(rows) != 1 {
		t.Fatalf("history row count = %d, want 1", len(rows))
	}
	if rows[0]["mam_id"] != "123" || rows[0]["title"] != "Direct Title" || rows[0]["media_type"] != mediaTypeAudiobook || rows[0]["dl"] != "direct-token" || rows[0]["torrent_status"] != "added" || rows[0]["torrent_hash"] != "HASH1" {
		t.Fatalf("unexpected history row: %#v", rows[0])
	}
}

func TestAddHandlerFallsBackToMAMTorrentFetchAndMetainfo(t *testing.T) {
	setupIsolatedState(t)

	torrentBytes := []byte("fake torrent bytes")
	mamCalls := 0
	mam := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		mamCalls++
		if r.URL.Path != "/tor/download.php" {
			t.Errorf("MAM fallback path = %s", r.URL.Path)
		}
		if got := r.URL.Query().Get("id"); got != "777" {
			t.Errorf("MAM fallback id query = %q, want 777", got)
		}
		if r.Header.Get("Cookie") != "mam_id=test-cookie" {
			t.Errorf("MAM fallback cookie = %q", r.Header.Get("Cookie"))
		}
		_, _ = w.Write(torrentBytes)
	}))
	defer mam.Close()

	transmissionCalls := 0
	transmission := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		transmissionCalls++
		req := decodeTransmissionRequest(t, r)
		if transmissionCalls == 1 {
			if stringFromAny(req.Arguments["filename"]) == "" {
				t.Errorf("first Transmission call missing filename: %#v", req.Arguments)
			}
			writeTransmissionResult(t, w, "invalid or corrupt torrent file", nil)
			return
		}
		if got := stringFromAny(req.Arguments["metainfo"]); got != base64.StdEncoding.EncodeToString(torrentBytes) {
			t.Errorf("metainfo = %q, want base64 torrent bytes", got)
		}
		writeTransmissionSuccess(t, w, map[string]any{
			"torrent-added": map[string]any{"hashString": "HASH2"},
		})
	}))
	defer transmission.Close()
	settingsRef.Store(testSettings(mam.URL, transmission.URL))

	rr := httptest.NewRecorder()
	addHandler(rr, jsonRequest(t, http.MethodPost, "/add", map[string]any{
		"id":         "777",
		"title":      "Fallback Title",
		"author":     "Author",
		"media_type": mediaTypeEbook,
		"dl":         "direct-token",
	}))
	assertStatus(t, rr, http.StatusOK)

	if transmissionCalls != 2 {
		t.Fatalf("Transmission calls = %d, want 2", transmissionCalls)
	}
	if mamCalls != 1 {
		t.Fatalf("MAM fallback calls = %d, want 1", mamCalls)
	}
	rows, err := sqliteQueryJSON(context.Background(), "SELECT media_type, torrent_hash FROM history WHERE mam_id = '777';")
	if err != nil {
		t.Fatalf("sqliteQueryJSON() error = %v", err)
	}
	if len(rows) != 1 || rows[0]["media_type"] != mediaTypeEbook || rows[0]["torrent_hash"] != "HASH2" {
		t.Fatalf("unexpected fallback history row: %#v", rows)
	}
}

func TestTransmissionRPCRetriesSessionIDConflict(t *testing.T) {
	setupIsolatedState(t)

	calls := 0
	transmission := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		calls++
		if calls == 1 {
			if got := r.Header.Get("X-Transmission-Session-Id"); got != "" {
				t.Errorf("first session id header = %q, want empty", got)
			}
			w.Header().Set("X-Transmission-Session-Id", "retry-session")
			w.WriteHeader(http.StatusConflict)
			return
		}
		if got := r.Header.Get("X-Transmission-Session-Id"); got != "retry-session" {
			t.Errorf("retry session id header = %q, want retry-session", got)
		}
		writeTransmissionSuccess(t, w, map[string]any{"value": "ok"})
	}))
	defer transmission.Close()
	settingsRef.Store(testSettings("https://mam.example", transmission.URL))

	args, err := transmissionRPC(context.Background(), transmission.Client(), "session-get", nil)
	if err != nil {
		t.Fatalf("transmissionRPC() error = %v", err)
	}
	if calls != 2 {
		t.Fatalf("calls = %d, want 2", calls)
	}
	if args["value"] != "ok" {
		t.Fatalf("args = %#v, want value ok", args)
	}
}

func TestTransmissionTorrentsHandlerFiltersCompletedLabel(t *testing.T) {
	setupIsolatedState(t)

	transmission := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		req := decodeTransmissionRequest(t, r)
		if req.Method != "torrent-get" {
			t.Errorf("method = %q, want torrent-get", req.Method)
		}
		writeTransmissionSuccess(t, w, map[string]any{
			"torrents": []map[string]any{
				{
					"hashString":  "HASH_OK",
					"name":        "Display Name",
					"percentDone": 1,
					"downloadDir": "/downloads",
					"totalSize":   123,
					"addedDate":   456,
					"labels":      []string{"test-label"},
					"files": []map[string]any{
						{"name": "Root/one.mp3"},
						{"name": "Root/two.mp3"},
					},
				},
				{
					"hashString":  "HASH_WRONG_LABEL",
					"name":        "Wrong Label",
					"percentDone": 1,
					"downloadDir": "/downloads",
					"labels":      []string{"other-label"},
					"files":       []map[string]any{{"name": "file.mp3"}},
				},
				{
					"hashString":  "HASH_INCOMPLETE",
					"name":        "Incomplete",
					"percentDone": 0.5,
					"downloadDir": "/downloads",
					"labels":      []string{"test-label"},
					"files":       []map[string]any{{"name": "file.mp3"}},
				},
			},
		})
	}))
	defer transmission.Close()
	settingsRef.Store(testSettings("https://mam.example", transmission.URL))

	rr := httptest.NewRecorder()
	transmissionTorrentsHandler(rr, httptest.NewRequest(http.MethodGet, "/transmission/torrents", nil))
	assertStatus(t, rr, http.StatusOK)

	var out struct {
		Items []completedTorrent `json:"items"`
	}
	decodeRecorder(t, rr, &out)
	if len(out.Items) != 1 {
		t.Fatalf("items = %#v, want one filtered torrent", out.Items)
	}
	if out.Items[0].Hash != "HASH_OK" || out.Items[0].Root != "Root" || out.Items[0].SingleFile {
		t.Fatalf("unexpected torrent item: %#v", out.Items[0])
	}
}

func TestImportHandlerRejectsDownloadDirOutsideDownloads(t *testing.T) {
	setupIsolatedState(t)

	transmission := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		writeTransmissionSuccess(t, w, map[string]any{
			"torrents": []map[string]any{
				{
					"hashString":  "HASH_BAD_PATH",
					"name":        "Book",
					"downloadDir": "/outside-downloads",
					"labels":      []string{"test-label"},
					"files":       []map[string]any{{"name": "Book/track.mp3"}},
				},
			},
		})
	}))
	defer transmission.Close()
	settingsRef.Store(testSettings("https://mam.example", transmission.URL))

	rr := httptest.NewRecorder()
	importHandler(rr, jsonRequest(t, http.MethodPost, "/import", map[string]any{
		"author":     "Author",
		"title":      "Title",
		"hash":       "HASH_BAD_PATH",
		"media_type": mediaTypeAudiobook,
	}))

	assertStatus(t, rr, http.StatusBadRequest)
	var out map[string]string
	decodeRecorder(t, rr, &out)
	if !strings.Contains(out["detail"], "expects completed downloads under /downloads") {
		t.Fatalf("detail = %q", out["detail"])
	}
}

type transmissionRequest struct {
	Method    string         `json:"method"`
	Arguments map[string]any `json:"arguments"`
}

func setupIsolatedState(t *testing.T) string {
	t.Helper()
	autoState.stop()

	oldConfigPath := configPath
	oldHistoryDBPath := historyDBPath
	oldSettings := settingsRef.Load()

	dir := t.TempDir()
	configPath = filepath.Join(dir, "config.json")
	historyDBPath = filepath.Join(dir, "history.db")

	for _, key := range []string{
		"MAM_BASE",
		"MAM_COOKIE",
		"TRANSMISSION_URL",
		"TRANSMISSION_USER",
		"TRANSMISSION_PASS",
		"TRANSMISSION_LABEL",
		"AUTO_IMPORT_ENABLED",
		"AUTO_IMPORT_POLL_INTERVAL",
		"DISABLE_SETUP",
	} {
		t.Setenv(key, "")
	}

	settingsRef.Store(loadSettings())
	if err := openDB(); err != nil {
		t.Fatalf("openDB() error = %v", err)
	}
	if err := ensureHistorySchema(); err != nil {
		t.Fatalf("ensureHistorySchema() error = %v", err)
	}

	t.Cleanup(func() {
		autoState.stop()
		configPath = oldConfigPath
		historyDBPath = oldHistoryDBPath
		settingsRef.Store(oldSettings)
	})

	return dir
}

func testSettings(mamBase, transmissionURL string) *AppSettings {
	return &AppSettings{
		MAMBase:               strings.TrimRight(mamBase, "/"),
		MAMCookie:             "mam_id=test-cookie",
		TransmissionURL:       strings.TrimRight(transmissionURL, "/"),
		TransmissionLabel:     "test-label",
		DownloadsDir:          downloadsDir,
		LibraryDir:            libraryDir,
		EbooksDir:             ebooksDir,
		AutoImportPollSeconds: defaultAutoImportPollSeconds,
	}
}

func jsonRequest(t *testing.T, method, target string, body any) *http.Request {
	t.Helper()
	data, err := json.Marshal(body)
	if err != nil {
		t.Fatalf("json.Marshal() error = %v", err)
	}
	req := httptest.NewRequest(method, target, bytes.NewReader(data))
	req.Header.Set("Content-Type", "application/json")
	return req
}

func assertStatus(t *testing.T, rr *httptest.ResponseRecorder, want int) {
	t.Helper()
	if rr.Code != want {
		t.Fatalf("status = %d, want %d; body: %s", rr.Code, want, rr.Body.String())
	}
}

func decodeRecorder(t *testing.T, rr *httptest.ResponseRecorder, dst any) {
	t.Helper()
	if err := json.NewDecoder(rr.Body).Decode(dst); err != nil {
		t.Fatalf("decode response %q: %v", rr.Body.String(), err)
	}
}

func decodeTransmissionRequest(t *testing.T, r *http.Request) transmissionRequest {
	t.Helper()
	var req transmissionRequest
	if err := json.NewDecoder(r.Body).Decode(&req); err != nil {
		t.Fatalf("decode Transmission request: %v", err)
	}
	if req.Arguments == nil {
		req.Arguments = map[string]any{}
	}
	return req
}

func writeTransmissionSuccess(t *testing.T, w http.ResponseWriter, args map[string]any) {
	t.Helper()
	writeTransmissionResult(t, w, "success", args)
}

func writeTransmissionResult(t *testing.T, w http.ResponseWriter, result string, args map[string]any) {
	t.Helper()
	w.Header().Set("Content-Type", "application/json")
	if args == nil {
		args = map[string]any{}
	}
	if err := json.NewEncoder(w).Encode(map[string]any{
		"result":    result,
		"arguments": args,
	}); err != nil {
		t.Fatalf("encode Transmission response: %v", err)
	}
}

func sameStrings(got, want []string) bool {
	if len(got) != len(want) {
		return false
	}
	seen := make(map[string]int, len(got))
	for _, value := range got {
		seen[value]++
	}
	for _, value := range want {
		if seen[value] == 0 {
			return false
		}
		seen[value]--
	}
	return true
}
