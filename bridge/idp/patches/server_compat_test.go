// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

package server

import (
	"fmt"
	"net/http/httptest"
	"net/url"
	"testing"

	"github.com/dexidp/dex/storage"
)

func TestKarsAuthorizationErrorDescriptionsLiteral(t *testing.T) {
	for _, tc := range []struct {
		name, method, redirectURI, responseType, description string
	}{
		{
			name: "percent verbs in PKCE method", method: "%s%[1]s%%",
			redirectURI: "https://example.invalid/callback", responseType: "code",
			description: `Unsupported PKCE challenge method ("%s%[1]s%%").`,
		},
		{
			name: "ordinary unsupported PKCE method", method: "unsupported",
			redirectURI: "https://example.invalid/callback", responseType: "code",
			description: `Unsupported PKCE challenge method ("unsupported").`,
		},
		{
			name: "token with out-of-band redirect", method: codeChallengeMethodS256,
			redirectURI: redirectURIOOB, responseType: "code token",
			description: fmt.Sprintf("Cannot use response type 'token' with redirect_uri '%s'.", redirectURIOOB),
		},
	} {
		t.Run(tc.name, func(t *testing.T) {
			httpServer, server := newTestServerMultipleConnectors(t, func(c *Config) {
				c.SupportedResponseTypes = []string{"code", "token"}
				c.Storage = storage.WithStaticClients(c.Storage, []storage.Client{{
					ID: "compat-client", RedirectURIs: []string{tc.redirectURI},
				}})
			})
			defer httpServer.Close()
			params := url.Values{
				"client_id": {"compat-client"}, "redirect_uri": {tc.redirectURI},
				"response_type": {tc.responseType}, "scope": {"openid"},
				"state": {"literal-state%25"}, "code_challenge_method": {tc.method},
				"code_challenge": {"challenge"},
			}
			req := httptest.NewRequest("GET", httpServer.URL+"/auth?"+params.Encode(), nil)
			_, err := server.parseAuthorizationRequest(req)
			redirected, ok := err.(*redirectedAuthErr)
			if !ok {
				t.Fatalf("expected redirectedAuthErr, got %T: %v", err, err)
			}
			if redirected.Type != errInvalidRequest || redirected.RedirectURI != tc.redirectURI ||
				redirected.State != params.Get("state") || redirected.Description != tc.description {
				t.Fatalf("redirected error = %+v; expected unchanged type/state/URI and literal description %q",
					redirected, tc.description)
			}
		})
	}
}
