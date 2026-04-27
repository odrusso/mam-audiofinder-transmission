//go:build integration

package main

import (
	"bytes"
	"encoding/json"
	"io"
	"net/http"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

func TestLiveMAMSearchReadOnly(t *testing.T) {
	loadEnvTestLocal(t)

	if os.Getenv("MAM_LIVE_TEST") != "1" {
		t.Skip("set MAM_LIVE_TEST=1 to run live MAM integration tests")
	}
	cookie := buildMAMCookie(os.Getenv("MAM_COOKIE"))
	if cookie == "" {
		t.Skip("set MAM_COOKIE to run live MAM integration tests")
	}

	base := strings.TrimRight(os.Getenv("MAM_BASE"), "/")
	if base == "" {
		base = defaultMAMBase
	}

	payload := map[string]any{
		"tor": map[string]any{
			"text":        "the",
			"srchIn":      []string{"title", "author", "narrator"},
			"searchType":  "all",
			"sortType":    "seedersDesc",
			"startNumber": "0",
			"main_cat":    []string{mamMainCategory(mediaTypeAudiobook)},
		},
		"perpage": 1,
	}
	body, err := json.Marshal(payload)
	if err != nil {
		t.Fatalf("json.Marshal() error = %v", err)
	}

	req, err := http.NewRequest(http.MethodPost, base+"/tor/js/loadSearchJSONbasic.php", bytes.NewReader(body))
	if err != nil {
		t.Fatalf("NewRequest() error = %v", err)
	}
	req.Header.Set("Cookie", cookie)
	req.Header.Set("Content-Type", "application/json")
	req.Header.Set("Accept", "application/json, */*")
	req.Header.Set("Origin", base)
	req.Header.Set("Referer", base+"/")
	req.Header.Set("User-Agent", "mam-audiofinder-transmission integration test")

	client := &http.Client{Timeout: 20 * time.Second}
	resp, err := client.Do(req)
	if err != nil {
		t.Fatalf("MAM search request failed: %v", err)
	}
	defer resp.Body.Close()

	if resp.StatusCode != http.StatusOK {
		bodyBytes, _ := io.ReadAll(io.LimitReader(resp.Body, 300))
		t.Fatalf("MAM status = %d, want 200; body: %s", resp.StatusCode, string(bodyBytes))
	}

	var raw map[string]json.RawMessage
	if err := json.NewDecoder(resp.Body).Decode(&raw); err != nil {
		t.Fatalf("MAM returned invalid JSON: %v", err)
	}
	if len(raw) == 0 {
		t.Fatalf("MAM returned an empty JSON object")
	}
	if _, ok := raw["data"]; !ok {
		if _, hasTotal := raw["total"]; !hasTotal {
			if _, hasTotalFound := raw["total_found"]; !hasTotalFound {
				t.Fatalf("MAM response missing expected top-level data/total fields: keys=%v", mapKeys(raw))
			}
		}
	}

	dataRaw, ok := raw["data"]
	if !ok || string(dataRaw) == "null" {
		return
	}
	var results []map[string]any
	if err := json.Unmarshal(dataRaw, &results); err != nil {
		t.Fatalf("MAM data field is not a result array: %v", err)
	}
	if len(results) == 0 {
		return
	}
	first := results[0]
	if stringFromAny(firstNonEmpty(first["id"], first["tid"])) == "" {
		t.Fatalf("first MAM result missing id/tid: %#v", first)
	}
	if stringFromAny(firstNonEmpty(first["title"], first["name"])) == "" {
		t.Fatalf("first MAM result missing title/name: %#v", first)
	}
	if stringFromAny(firstNonEmpty(first["catname"], first["category"])) == "" {
		t.Fatalf("first MAM result missing category/catname: %#v", first)
	}
}

func loadEnvTestLocal(t *testing.T) {
	t.Helper()

	data, err := os.ReadFile(filepath.Join(".", ".env.test.local"))
	if err != nil {
		if os.IsNotExist(err) {
			return
		}
		t.Fatalf("read .env.test.local: %v", err)
	}

	for _, line := range strings.Split(string(data), "\n") {
		line = strings.TrimSpace(line)
		if line == "" || strings.HasPrefix(line, "#") {
			continue
		}
		key, value, ok := strings.Cut(line, "=")
		if !ok {
			continue
		}
		key = strings.TrimSpace(key)
		value = strings.TrimSpace(value)
		value = strings.Trim(value, `"'`)
		if key == "" {
			continue
		}
		if _, exists := os.LookupEnv(key); !exists {
			t.Setenv(key, value)
		}
	}
}

func mapKeys(m map[string]json.RawMessage) []string {
	keys := make([]string, 0, len(m))
	for key := range m {
		keys = append(keys, key)
	}
	return keys
}
