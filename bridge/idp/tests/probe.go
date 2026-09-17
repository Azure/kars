// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// Hosted-only black-box checks against the actual distroless Dex container.
package main

import (
	"crypto"
	"crypto/rand"
	"crypto/rsa"
	"crypto/sha256"
	"encoding/base64"
	"encoding/json"
	"fmt"
	"io"
	"math/big"
	"net/http"
	"net/http/cookiejar"
	"net/url"
	"os"
	"path/filepath"
	"strings"
	"time"

	"golang.org/x/crypto/bcrypt"
	"golang.org/x/net/html"
)

const (
	clientID = "kars-packaging-probe"
	email    = "packaging-test@example.invalid"
	callback = "http://127.0.0.1:18999/callback"
)

type credentials struct {
	Password   string
	Refresh    string
	Nonce      string
	IDToken    string
	SigningKey signingKeyIdentity
}

type signingKeyIdentity struct {
	Kid      string
	Modulus  string
	Exponent int
}

type verifiedIDToken struct {
	Subject    string
	SigningKey signingKeyIdentity
}

type tokenSet struct {
	Access  string `json:"access_token"`
	ID      string `json:"id_token"`
	Refresh string `json:"refresh_token"`
	Type    string `json:"token_type"`
}

func randomToken() (string, error) {
	b := make([]byte, 32)
	if _, err := rand.Read(b); err != nil {
		return "", err
	}
	return base64.RawURLEncoding.EncodeToString(b), nil
}

func writeJSON(path string, value any, mode os.FileMode) error {
	b, err := json.MarshalIndent(value, "", "  ")
	if err != nil {
		return err
	}
	return os.WriteFile(path, b, mode)
}

func configure(issuer, dir, storage string) error {
	if storage != "memory" && storage != "sqlite3" {
		return fmt.Errorf("unsupported test storage %q", storage)
	}
	password, err := randomToken()
	if err != nil {
		return err
	}
	hash, err := bcrypt.GenerateFromPassword([]byte(password), bcrypt.DefaultCost)
	if err != nil {
		return err
	}
	store := map[string]any{"type": storage}
	if storage == "sqlite3" {
		store["config"] = map[string]any{"file": "/var/dex/dex.db"}
		if err := os.Chown("/var/dex", 1001, 1001); err != nil {
			return err
		}
	}
	config := map[string]any{
		"issuer": issuer, "storage": store,
		"web":      map[string]any{"http": "0.0.0.0:5556"},
		"frontend": map[string]any{"dir": "/srv/dex/web"},
		"oauth2":   map[string]any{"skipApprovalScreen": true},
		"staticClients": []any{map[string]any{
			"id": clientID, "name": "Ephemeral packaging test",
			"secretEnv": "DEX_TEST_CLIENT_SECRET", "redirectURIs": []string{callback},
		}},
		"enablePasswordDB": true,
		"staticPasswords": []any{map[string]any{
			"email": email, "hash": string(hash), "username": "packaging-test",
			"userID": "286e3ba6-cfb5-47e1-a5ae-66b290626b66",
		}},
	}
	if err := writeJSON(filepath.Join(dir, "config.yaml"), config, 0644); err != nil {
		return err
	}
	return writeJSON(filepath.Join(dir, "credentials.json"), credentials{Password: password}, 0600)
}

func client() (*http.Client, error) {
	jar, err := cookiejar.New(nil)
	if err != nil {
		return nil, err
	}
	return &http.Client{
		Jar: jar, Timeout: 10 * time.Second,
		CheckRedirect: func(req *http.Request, via []*http.Request) error {
			if strings.HasPrefix(req.URL.String(), callback+"?") {
				return http.ErrUseLastResponse
			}
			if len(via) >= 10 {
				return fmt.Errorf("too many redirects")
			}
			return nil
		},
	}, nil
}

func getJSON(c *http.Client, endpoint string, result any) error {
	resp, err := c.Get(endpoint)
	if err != nil {
		return err
	}
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusOK {
		return fmt.Errorf("GET %s returned %d", endpoint, resp.StatusCode)
	}
	return json.NewDecoder(io.LimitReader(resp.Body, 1<<20)).Decode(result)
}

func login(issuer, password, verifier, nonce string, shouldSucceed bool) (string, error) {
	c, err := client()
	if err != nil {
		return "", err
	}
	state, err := randomToken()
	if err != nil {
		return "", err
	}
	challenge := sha256.Sum256([]byte(verifier))
	query := url.Values{
		"client_id": {clientID}, "redirect_uri": {callback}, "response_type": {"code"},
		"scope": {"openid profile email offline_access"}, "state": {state}, "nonce": {nonce},
		"code_challenge": {base64.RawURLEncoding.EncodeToString(challenge[:])},
		"code_challenge_method": {"S256"}, "connector_id": {"local"},
	}
	resp, err := c.Get(issuer + "/auth?" + query.Encode())
	if err != nil {
		return "", err
	}
	doc, parseErr := html.Parse(io.LimitReader(resp.Body, 1<<20))
	resp.Body.Close()
	if parseErr != nil {
		return "", parseErr
	}
	if resp.StatusCode != http.StatusOK {
		return "", fmt.Errorf("login page returned %d", resp.StatusCode)
	}
	action := ""
	foundForm, foundPassword := false, false
	fields := url.Values{}
	var visit func(*html.Node)
	visit = func(n *html.Node) {
		attrs := map[string]string{}
		for _, a := range n.Attr {
			attrs[a.Key] = a.Val
		}
		if n.Type == html.ElementNode && n.Data == "form" {
			action, foundForm = attrs["action"], true
		}
		if n.Type == html.ElementNode && n.Data == "input" {
			if attrs["type"] == "hidden" {
				fields.Set(attrs["name"], attrs["value"])
			}
			if attrs["name"] == "password" {
				foundPassword = true
			}
		}
		for child := n.FirstChild; child != nil; child = child.NextSibling {
			visit(child)
		}
	}
	visit(doc)
	if !foundForm || !foundPassword {
		return "", fmt.Errorf("real password login form missing")
	}
	target, err := resp.Request.URL.Parse(action)
	if err != nil {
		return "", err
	}
	if target.Host != resp.Request.URL.Host || target.Scheme != resp.Request.URL.Scheme {
		return "", fmt.Errorf("login form leaves issuer origin")
	}
	fields.Set("login", email)
	fields.Set("password", password)
	resp, err = c.PostForm(target.String(), fields)
	if err != nil {
		return "", err
	}
	defer resp.Body.Close()
	location := resp.Header.Get("Location")
	if !shouldSucceed {
		if resp.StatusCode != http.StatusUnauthorized || strings.HasPrefix(location, callback) {
			return "", fmt.Errorf("invalid password did not return its unauthorized login form (status %d)", resp.StatusCode)
		}
		body, err := io.ReadAll(io.LimitReader(resp.Body, 1<<20))
		if err != nil {
			return "", err
		}
		if !strings.Contains(string(body), `id="login-error"`) {
			return "", fmt.Errorf("invalid password rejection missing")
		}
		return "", nil
	}
	if (resp.StatusCode != 302 && resp.StatusCode != 303) || !strings.HasPrefix(location, callback+"?") {
		return "", fmt.Errorf("authorization did not redirect to registered callback: status %d", resp.StatusCode)
	}
	redirect, err := url.Parse(location)
	if err != nil {
		return "", err
	}
	if redirect.Query().Get("state") != state || redirect.Query().Get("code") == "" || redirect.Query().Get("error") != "" {
		return "", fmt.Errorf("authorization code/state response invalid")
	}
	return redirect.Query().Get("code"), nil
}

func exchange(issuer, secret string, form url.Values, shouldSucceed bool) (tokenSet, error) {
	c, err := client()
	if err != nil {
		return tokenSet{}, err
	}
	req, err := http.NewRequest(http.MethodPost, issuer+"/token", strings.NewReader(form.Encode()))
	if err != nil {
		return tokenSet{}, err
	}
	req.Header.Set("Content-Type", "application/x-www-form-urlencoded")
	req.SetBasicAuth(clientID, secret)
	resp, err := c.Do(req)
	if err != nil {
		return tokenSet{}, err
	}
	defer resp.Body.Close()
	if !shouldSucceed {
		var rejection struct{ Error string }
		if err := json.NewDecoder(resp.Body).Decode(&rejection); err != nil {
			return tokenSet{}, err
		}
		if resp.StatusCode < 400 || resp.StatusCode >= 500 || rejection.Error == "" {
			return tokenSet{}, fmt.Errorf("invalid token request was not rejected")
		}
		return tokenSet{}, nil
	}
	if resp.StatusCode != http.StatusOK {
		return tokenSet{}, fmt.Errorf("token endpoint returned %d", resp.StatusCode)
	}
	var tokens tokenSet
	if err := json.NewDecoder(resp.Body).Decode(&tokens); err != nil {
		return tokens, err
	}
	if tokens.Access == "" || tokens.ID == "" || tokens.Refresh == "" || !strings.EqualFold(tokens.Type, "bearer") {
		return tokens, fmt.Errorf("incomplete token response")
	}
	return tokens, nil
}

func verifyIDToken(c *http.Client, issuer, nonce, idToken string) (verifiedIDToken, error) {
	parts := strings.Split(idToken, ".")
	if len(parts) != 3 {
		return verifiedIDToken{}, fmt.Errorf("invalid ID token shape")
	}
	decode := base64.RawURLEncoding.DecodeString
	headerBytes, err := decode(parts[0])
	if err != nil {
		return verifiedIDToken{}, err
	}
	var header struct{ Alg, Kid string }
	if err := json.Unmarshal(headerBytes, &header); err != nil {
		return verifiedIDToken{}, err
	}
	if header.Alg != "RS256" || header.Kid == "" {
		return verifiedIDToken{}, fmt.Errorf("unexpected JWT algorithm or missing kid")
	}
	var jwks struct{ Keys []struct{ Kty, Kid, N, E string } }
	if err := getJSON(c, issuer+"/keys", &jwks); err != nil {
		return verifiedIDToken{}, err
	}
	var key *rsa.PublicKey
	for _, jwk := range jwks.Keys {
		if jwk.Kid != header.Kid || jwk.Kty != "RSA" {
			continue
		}
		n, err := decode(jwk.N)
		if err != nil {
			return verifiedIDToken{}, err
		}
		e, err := decode(jwk.E)
		if err != nil {
			return verifiedIDToken{}, err
		}
		exponent := new(big.Int).SetBytes(e)
		if exponent.BitLen() > 31 || exponent.Int64() < 3 {
			return verifiedIDToken{}, fmt.Errorf("invalid RSA exponent")
		}
		key = &rsa.PublicKey{N: new(big.Int).SetBytes(n), E: int(exponent.Int64())}
	}
	if key == nil || key.N.BitLen() < 2048 {
		return verifiedIDToken{}, fmt.Errorf("matching strong JWKS signing key missing")
	}
	signature, err := decode(parts[2])
	if err != nil {
		return verifiedIDToken{}, err
	}
	digest := sha256.Sum256([]byte(parts[0] + "." + parts[1]))
	if err := rsa.VerifyPKCS1v15(key, crypto.SHA256, digest[:], signature); err != nil {
		return verifiedIDToken{}, err
	}
	payload, err := decode(parts[1])
	if err != nil {
		return verifiedIDToken{}, err
	}
	var claims struct {
		Iss, Sub, Nonce, Email string
		Aud                   json.RawMessage
		Exp                   int64
		EmailVerified         bool `json:"email_verified"`
	}
	if err := json.Unmarshal(payload, &claims); err != nil {
		return verifiedIDToken{}, err
	}
	var audience string
	var audiences []string
	if err := json.Unmarshal(claims.Aud, &audience); err != nil {
		if err := json.Unmarshal(claims.Aud, &audiences); err != nil {
			return verifiedIDToken{}, err
		}
		if len(audiences) == 1 {
			audience = audiences[0]
		}
	}
	if claims.Iss != issuer || audience != clientID || claims.Nonce != nonce ||
		claims.Exp <= time.Now().Unix() || claims.Sub == "" || claims.Email != email || !claims.EmailVerified {
		return verifiedIDToken{}, fmt.Errorf("signed ID token claims mismatch")
	}
	return verifiedIDToken{
		Subject: claims.Sub,
		SigningKey: signingKeyIdentity{
			Kid: header.Kid, Modulus: base64.RawURLEncoding.EncodeToString(key.N.Bytes()), Exponent: key.E,
		},
	}, nil
}

func verify(issuer, nonce string, tokens tokenSet) (signingKeyIdentity, error) {
	c, err := client()
	if err != nil {
		return signingKeyIdentity{}, err
	}
	verified, err := verifyIDToken(c, issuer, nonce, tokens.ID)
	if err != nil {
		return signingKeyIdentity{}, err
	}
	req, err := http.NewRequest(http.MethodGet, issuer+"/userinfo", nil)
	if err != nil {
		return signingKeyIdentity{}, err
	}
	req.Header.Set("Authorization", "Bearer "+tokens.Access)
	resp, err := c.Do(req)
	if err != nil {
		return signingKeyIdentity{}, err
	}
	defer resp.Body.Close()
	var user struct{ Sub, Email string }
	if resp.StatusCode != 200 {
		return signingKeyIdentity{}, fmt.Errorf("userinfo returned %d", resp.StatusCode)
	}
	if err := json.NewDecoder(resp.Body).Decode(&user); err != nil {
		return signingKeyIdentity{}, err
	}
	if user.Sub != verified.Subject || user.Email != email {
		return signingKeyIdentity{}, fmt.Errorf("userinfo identity mismatch")
	}
	return verified.SigningKey, nil
}

func check(issuer, dir, mode string) error {
	c, err := client()
	if err != nil {
		return err
	}
	var discovery struct {
		Issuer string
		Auth   string   `json:"authorization_endpoint"`
		Token  string   `json:"token_endpoint"`
		JWKS   string   `json:"jwks_uri"`
		PKCE   []string `json:"code_challenge_methods_supported"`
	}
	var readyErr error
	for attempt := 0; attempt < 60; attempt++ {
		readyErr = getJSON(c, issuer+"/.well-known/openid-configuration", &discovery)
		if readyErr == nil {
			break
		}
		time.Sleep(time.Second)
	}
	if readyErr != nil {
		return fmt.Errorf("discovery never became ready: %w", readyErr)
	}
	if discovery.Issuer != issuer || discovery.Auth != issuer+"/auth" ||
		discovery.Token != issuer+"/token" || discovery.JWKS != issuer+"/keys" ||
		!strings.Contains(strings.Join(discovery.PKCE, ","), "S256") {
		return fmt.Errorf("OIDC discovery contract mismatch")
	}
	var creds credentials
	data, err := os.ReadFile(filepath.Join(dir, "credentials.json"))
	if err != nil {
		return err
	}
	if err := json.Unmarshal(data, &creds); err != nil {
		return err
	}
	secret := os.Getenv("DEX_TEST_CLIENT_SECRET")
	if secret == "" {
		return fmt.Errorf("ephemeral client secret missing")
	}
	if mode == "resume" {
		if creds.IDToken == "" || creds.SigningKey.Kid == "" || creds.SigningKey.Modulus == "" || creds.SigningKey.Exponent == 0 {
			return fmt.Errorf("pre-restart signing-key continuity evidence missing")
		}
		previous, err := verifyIDToken(c, issuer, creds.Nonce, creds.IDToken)
		if err != nil {
			return fmt.Errorf("pre-restart ID token verification failed: %w", err)
		}
		if previous.SigningKey != creds.SigningKey {
			return fmt.Errorf("pre-restart signing-key identity changed")
		}
		tokens, err := exchange(issuer, secret, url.Values{
			"grant_type": {"refresh_token"}, "refresh_token": {creds.Refresh},
		}, true)
		if err != nil {
			return err
		}
		_, err = verify(issuer, creds.Nonce, tokens)
		return err
	}
	verifier, err := randomToken()
	if err != nil {
		return err
	}
	creds.Nonce, err = randomToken()
	if err != nil {
		return err
	}
	if _, err := login(issuer, "deliberately-wrong", verifier, creds.Nonce, false); err != nil {
		return err
	}
	for _, scenario := range []string{"wrong-pkce", "wrong-secret", "valid"} {
		code, err := login(issuer, creds.Password, verifier, creds.Nonce, true)
		if err != nil {
			return err
		}
		form := url.Values{
			"grant_type": {"authorization_code"}, "redirect_uri": {callback},
			"code": {code}, "code_verifier": {verifier},
		}
		useSecret := secret
		if scenario == "wrong-pkce" {
			form.Set("code_verifier", strings.Repeat("x", 43))
		}
		if scenario == "wrong-secret" {
			useSecret = "deliberately-wrong"
		}
		tokens, err := exchange(issuer, useSecret, form, scenario == "valid")
		if err != nil {
			return fmt.Errorf("%s: %w", scenario, err)
		}
		if scenario == "valid" {
			signingKey, err := verify(issuer, creds.Nonce, tokens)
			if err != nil {
				return err
			}
			if _, err := exchange(issuer, secret, form, false); err != nil {
				return fmt.Errorf("authorization code replay: %w", err)
			}
			creds.Refresh = tokens.Refresh
			creds.IDToken = tokens.ID
			creds.SigningKey = signingKey
		}
	}
	return writeJSON(filepath.Join(dir, "credentials.json"), creds, 0600)
}

func run() error {
	if len(os.Args) < 4 {
		return fmt.Errorf("usage: probe config|check|resume issuer config-directory [memory|sqlite3]")
	}
	switch os.Args[1] {
	case "config":
		if len(os.Args) != 5 {
			return fmt.Errorf("config requires storage")
		}
		return configure(os.Args[2], os.Args[3], os.Args[4])
	case "check", "resume":
		return check(os.Args[2], os.Args[3], os.Args[1])
	default:
		return fmt.Errorf("unknown probe command")
	}
}

func main() {
	if err := run(); err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
	fmt.Println("Dex packaging probe passed")
}
