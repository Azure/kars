// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

package google

import (
	"encoding/json"
	"fmt"
	"net"
	"net/http"
	"net/http/httptest"
	"net/url"
	"os"
	"testing"
)

// Installed only through the qualification overlay, never in the source tree.
// Config.Open uses a background context, so its discovery client cannot be
// injected by newConnector. Set the transport once, before any tests run.
func TestMain(m *testing.M) {
	os.Exit(karsRunGoogleTests(m))
}

func karsRunGoogleTests(m *testing.M) int {
	original := http.DefaultTransport
	local := original.(*http.Transport).Clone()
	local.Proxy = nil
	defer local.CloseIdleConnections()

	discovery := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.Method != http.MethodGet || r.URL.RequestURI() != "/.well-known/openid-configuration" {
			http.Error(w, "unexpected discovery request", http.StatusBadRequest)
			return
		}
		w.Header().Set("Content-Type", "application/json")
		json.NewEncoder(w).Encode(map[string]any{
			"issuer":                                issuerURL,
			"authorization_endpoint":                issuerURL + "/o/oauth2/v2/auth",
			"token_endpoint":                        "https://oauth2.googleapis.com/token",
			"jwks_uri":                              "https://www.googleapis.com/oauth2/v3/certs",
			"response_types_supported":              []string{"code"},
			"subject_types_supported":               []string{"public"},
			"id_token_signing_alg_values_supported": []string{"RS256"},
		})
	}))
	defer discovery.Close()
	target, err := url.Parse(discovery.URL)
	if err != nil {
		fmt.Fprintln(os.Stderr, err)
		return 1
	}
	http.DefaultTransport = karsGoogleTestTransport{local: local, discovery: target}
	defer func() { http.DefaultTransport = original }()
	return m.Run()
}

type karsGoogleTestTransport struct {
	local     http.RoundTripper
	discovery *url.URL
}

func (t karsGoogleTestTransport) RoundTrip(r *http.Request) (*http.Response, error) {
	if r.Method == http.MethodGet && r.URL.String() == issuerURL+"/.well-known/openid-configuration" {
		request := r.Clone(r.Context())
		request.URL.Scheme = t.discovery.Scheme
		request.URL.Host = t.discovery.Host
		request.Host = ""
		return t.local.RoundTrip(request)
	}
	// Existing upstream Directory and GCE metadata fixtures bind numeric
	// loopback addresses. Do not resolve hostnames or fall back to the network.
	if r.URL.Scheme == "http" && net.ParseIP(r.URL.Hostname()).IsLoopback() {
		return t.local.RoundTrip(r)
	}
	return nil, fmt.Errorf("Google qualification forbids external request: %s %s", r.Method, r.URL.Redacted())
}

func TestKarsGoogleDiscoveryIsolation(t *testing.T) {
	request, err := http.NewRequest(http.MethodGet, issuerURL+"/.well-known/openid-configuration", nil)
	if err != nil {
		t.Fatal(err)
	}
	originalURL := request.URL.String()
	response, err := http.DefaultClient.Do(request)
	if err != nil {
		t.Fatal(err)
	}
	defer response.Body.Close()
	var document struct {
		Issuer        string `json:"issuer"`
		Authorization string `json:"authorization_endpoint"`
		Token         string `json:"token_endpoint"`
		JWKS          string `json:"jwks_uri"`
	}
	if err := json.NewDecoder(response.Body).Decode(&document); err != nil {
		t.Fatal(err)
	}
	if response.StatusCode != http.StatusOK || document.Issuer != issuerURL ||
		document.Authorization != issuerURL+"/o/oauth2/v2/auth" ||
		document.Token != "https://oauth2.googleapis.com/token" ||
		document.JWKS != "https://www.googleapis.com/oauth2/v3/certs" {
		t.Fatalf("unexpected discovery response: status=%d document=%+v", response.StatusCode, document)
	}
	if request.URL.String() != originalURL {
		t.Fatal("discovery transport mutated the caller's request")
	}
	for _, address := range []string{
		issuerURL + "/.well-known/openid-configuration?unexpected=1",
		issuerURL + "/unexpected",
		document.Token,
		document.JWKS,
		"https://unexpected.invalid/",
		"http://localhost/",
		"http://169.254.169.254/computeMetadata/v1/",
	} {
		t.Run(address, func(t *testing.T) {
			request, err := http.NewRequest(http.MethodGet, address, nil)
			if err != nil {
				t.Fatal(err)
			}
			response, err := http.DefaultTransport.RoundTrip(request)
			if response != nil {
				response.Body.Close()
			}
			if err == nil || response != nil {
				t.Fatalf("unexpected destination was not rejected: response=%v error=%v", response, err)
			}
		})
	}
	request.Method = http.MethodPost
	if _, err := http.DefaultTransport.RoundTrip(request); err == nil {
		t.Fatal("non-GET discovery request was not rejected")
	}
}

func TestKarsGoogleLoopbackFixtures(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.WriteHeader(http.StatusNoContent)
	}))
	defer server.Close()
	response, err := http.Get(server.URL)
	if err != nil {
		t.Fatal(err)
	}
	defer response.Body.Close()
	if response.StatusCode != http.StatusNoContent {
		t.Fatalf("loopback fixture status: %d", response.StatusCode)
	}
}
