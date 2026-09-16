// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

package main

import (
	"crypto"
	"crypto/rand"
	"crypto/rsa"
	"crypto/sha256"
	"encoding/base64"
	"encoding/json"
	"math/big"
	"net/http"
	"net/http/httptest"
	"net/url"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"sync"
	"testing"
	"time"
)

type continuityJWK struct {
	Kty string `json:"kty"`
	Kid string `json:"kid"`
	N   string `json:"n"`
	E   string `json:"e"`
}

func continuityKey(kid string, key *rsa.PrivateKey) continuityJWK {
	return continuityJWK{
		Kty: "RSA", Kid: kid,
		N: base64.RawURLEncoding.EncodeToString(key.N.Bytes()),
		E: base64.RawURLEncoding.EncodeToString(big.NewInt(int64(key.E)).Bytes()),
	}
}

func continuityToken(t *testing.T, key *rsa.PrivateKey, issuer, kid, nonce string, expires time.Time) string {
	t.Helper()
	header, err := json.Marshal(map[string]string{"alg": "RS256", "kid": kid})
	if err != nil {
		t.Fatal(err)
	}
	claims, err := json.Marshal(map[string]any{
		"iss": issuer, "aud": clientID, "sub": "continuity-subject", "nonce": nonce,
		"exp": expires.Unix(), "email": email, "email_verified": true,
	})
	if err != nil {
		t.Fatal(err)
	}
	unsigned := base64.RawURLEncoding.EncodeToString(header) + "." + base64.RawURLEncoding.EncodeToString(claims)
	digest := sha256.Sum256([]byte(unsigned))
	signature, err := rsa.SignPKCS1v15(rand.Reader, key, crypto.SHA256, digest[:])
	if err != nil {
		t.Fatal(err)
	}
	return unsigned + "." + base64.RawURLEncoding.EncodeToString(signature)
}

func continuityJSON(t *testing.T, w http.ResponseWriter, value any) {
	t.Helper()
	w.Header().Set("Content-Type", "application/json")
	if err := json.NewEncoder(w).Encode(value); err != nil {
		t.Errorf("fixture JSON response: %v", err)
	}
}

func TestSigningKeyContinuity(t *testing.T) {
	oldKey, err := rsa.GenerateKey(rand.Reader, 2048)
	if err != nil {
		t.Fatal(err)
	}
	newKey, err := rsa.GenerateKey(rand.Reader, 2048)
	if err != nil {
		t.Fatal(err)
	}
	const secret = "ephemeral-unit-test-client-secret"
	const nonce = "pre-restart-nonce"
	t.Setenv("DEX_TEST_CLIENT_SECRET", secret)

	for _, tc := range []struct {
		name            string
		retainOldKey    bool
		sameKid         bool
		unchanged       bool
		changedIdentity bool
		missingToken    bool
		missingIdentity bool
		expiredToken    bool
		wrongUserinfo   bool
		wantError       string
	}{
		{name: "lost signing keys despite working refresh", wantError: "pre-restart ID token verification failed"},
		{name: "replacement key reuses old kid", sameKid: true, wantError: "pre-restart ID token verification failed"},
		{name: "rotation retains old verification key", retainOldKey: true},
		{name: "unchanged signing key", unchanged: true},
		{name: "saved key identity differs", retainOldKey: true, changedIdentity: true, wantError: "pre-restart signing-key identity changed"},
		{name: "missing old token", retainOldKey: true, missingToken: true, wantError: "continuity evidence missing"},
		{name: "missing verified key identity", retainOldKey: true, missingIdentity: true, wantError: "continuity evidence missing"},
		{name: "expired old token", retainOldKey: true, expiredToken: true, wantError: "signed ID token claims mismatch"},
		{name: "refresh still requires matching userinfo", retainOldKey: true, wrongUserinfo: true, wantError: "userinfo identity mismatch"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			var mu sync.Mutex
			var issuer string
			var requests []string
			keys := []continuityJWK{continuityKey("old-key", oldKey)}
			var refreshed tokenSet
			userSubject := "continuity-subject"
			server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
				mu.Lock()
				defer mu.Unlock()
				requests = append(requests, r.URL.Path)
				switch r.URL.Path {
				case "/dex/.well-known/openid-configuration":
					continuityJSON(t, w, map[string]any{
						"issuer": issuer, "authorization_endpoint": issuer + "/auth",
						"token_endpoint": issuer + "/token", "jwks_uri": issuer + "/keys",
						"code_challenge_methods_supported": []string{"S256"},
					})
				case "/dex/keys":
					continuityJSON(t, w, map[string]any{"keys": keys})
				case "/dex/token":
					id, password, ok := r.BasicAuth()
					if r.Method != http.MethodPost || !ok || id != clientID || password != secret {
						http.Error(w, "fixture client authentication failed", http.StatusUnauthorized)
						return
					}
					if err := r.ParseForm(); err != nil || r.Form.Get("grant_type") != "refresh_token" ||
						r.Form.Get("refresh_token") != "persisted-refresh" {
						http.Error(w, "fixture refresh request invalid", http.StatusBadRequest)
						return
					}
					continuityJSON(t, w, refreshed)
				case "/dex/userinfo":
					if auth := r.Header.Get("Authorization"); auth != "Bearer old-access" && auth != "Bearer new-access" {
						http.Error(w, "fixture access token invalid", http.StatusUnauthorized)
						return
					}
					continuityJSON(t, w, map[string]string{"sub": userSubject, "email": email})
				default:
					http.NotFound(w, r)
				}
			}))
			defer server.Close()
			mu.Lock()
			issuer = server.URL + "/dex"
			mu.Unlock()

			oldTokens := tokenSet{
				Access: "old-access", Refresh: "persisted-refresh", Type: "Bearer",
				ID: continuityToken(t, oldKey, issuer, "old-key", nonce, time.Now().Add(time.Hour)),
			}
			identity, err := verify(issuer, nonce, oldTokens)
			if err != nil {
				t.Fatalf("pre-restart token verification: %v", err)
			}
			expectedIdentity := signingKeyIdentity{
				Kid: "old-key", Modulus: continuityKey("old-key", oldKey).N, Exponent: oldKey.E,
			}
			if identity != expectedIdentity {
				t.Fatalf("verified signing-key identity = %+v, want %+v", identity, expectedIdentity)
			}
			creds := credentials{
				Password: "unused-by-resume", Refresh: oldTokens.Refresh, Nonce: nonce,
				IDToken: oldTokens.ID, SigningKey: identity,
			}
			if tc.changedIdentity {
				creds.SigningKey.Modulus = continuityKey("new-key", newKey).N
			}
			if tc.missingToken {
				creds.IDToken = ""
			}
			if tc.missingIdentity {
				creds.SigningKey = signingKeyIdentity{}
			}
			if tc.expiredToken {
				creds.IDToken = continuityToken(t, oldKey, issuer, "old-key", nonce, time.Now().Add(-time.Hour))
			}
			dir := t.TempDir()
			path := filepath.Join(dir, "credentials.json")
			if err := writeJSON(path, creds, 0600); err != nil {
				t.Fatal(err)
			}
			info, err := os.Stat(path)
			if err != nil {
				t.Fatal(err)
			}
			if info.Mode().Perm() != 0600 {
				t.Fatalf("private continuity credentials mode = %o, want 600", info.Mode().Perm())
			}

			activeKey, activeKid := newKey, "new-key"
			if tc.sameKid {
				activeKid = "old-key"
			}
			if tc.unchanged {
				activeKey, activeKid = oldKey, "old-key"
			}
			newTokens := tokenSet{
				Access: "new-access", Refresh: "renewed-refresh", Type: "Bearer",
				ID: continuityToken(t, activeKey, issuer, activeKid, nonce, time.Now().Add(time.Hour)),
			}
			mu.Lock()
			keys = []continuityJWK{continuityKey(activeKid, activeKey)}
			if tc.retainOldKey {
				keys = append(keys, continuityKey("old-key", oldKey))
			}
			refreshed = newTokens
			mu.Unlock()

			// Prove the previous refresh-only check would pass, including when
			// the old signing key has been lost or replaced under the same kid.
			control, err := exchange(issuer, secret, url.Values{
				"grant_type": {"refresh_token"}, "refresh_token": {creds.Refresh},
			}, true)
			if err != nil {
				t.Fatalf("persisted refresh control: %v", err)
			}
			if _, err := verify(issuer, nonce, control); err != nil {
				t.Fatalf("new-token/current-JWKS control: %v", err)
			}
			mu.Lock()
			requests = nil
			if tc.wrongUserinfo {
				userSubject = "different-user"
			}
			mu.Unlock()

			err = check(issuer, dir, "resume")
			if tc.wantError == "" {
				if err != nil {
					t.Fatalf("retained signing key must survive resume: %v", err)
				}
			} else if err == nil || !strings.Contains(err.Error(), tc.wantError) {
				t.Fatalf("resume error = %v, want %q", err, tc.wantError)
			}
			mu.Lock()
			gotRequests := append([]string(nil), requests...)
			mu.Unlock()
			wantRequests := []string{"/dex/.well-known/openid-configuration"}
			if !tc.missingToken && !tc.missingIdentity {
				wantRequests = append(wantRequests, "/dex/keys")
			}
			if tc.wantError == "" || tc.wrongUserinfo {
				wantRequests = append(wantRequests, "/dex/token", "/dex/keys", "/dex/userinfo")
			}
			if !slices.Equal(gotRequests, wantRequests) {
				t.Fatalf("request order = %v, want %v; old-token verification must precede refresh", gotRequests, wantRequests)
			}
		})
	}
}
