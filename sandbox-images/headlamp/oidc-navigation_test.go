// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

package main

import (
	"net/url"
	"testing"
)

func TestOidcCallbackPath(t *testing.T) {
	for _, tc := range []struct{ cluster, route, want string }{
		{"main", "", "auth?cluster=main"},
		{"main", "/", "c/main/"},
		{"main", "/kars/sre", "c/main/kars/sre"},
		{"other cluster", "/kars/mesh", "c/other%20cluster/kars/mesh"},
		{"a/b", "/", "c/a%2Fb/"},
		{"other cluster", "", "auth?cluster=other+cluster"},
		{"main", "https://attacker.test/", "auth?cluster=main"},
		{"main", "//attacker.test/", "auth?cluster=main"},
		{"main", "/../outside", "auth?cluster=main"},
		{"main", "/%2e%2e/outside", "auth?cluster=main"},
		{"main", "/kars\\outside", "auth?cluster=main"},
		{"main", "/kars/sre?redirect=https://attacker.test", "auth?cluster=main"},
		{"main", "/kars/sre#fragment", "auth?cluster=main"},
		{"main", "/kars/sre\n", "auth?cluster=main"},
		{"..", "/", "auth?cluster=.."},
	} {
		t.Run(tc.cluster+tc.route, func(t *testing.T) {
			if got := oidcCallbackPath(tc.cluster, tc.route); got != tc.want {
				t.Fatalf("got %q, want %q", got, tc.want)
			}
		})
	}
}

func TestOidcTopLevelReturn(t *testing.T) {
	for _, route := range []string{"/", "/kars/sre", "/kars/karssandboxes", "/kars/team-profiles"} {
		if got := oidcTopLevelReturn(url.Values{"return": {route}}); got != route {
			t.Fatalf("valid route %q rejected", route)
		}
	}
	for _, route := range []string{"", "//example.test", "/../auth", "/%2foutside", "/kars\nsre"} {
		if got := oidcTopLevelReturn(url.Values{"return": {route}}); got != "" {
			t.Fatalf("invalid route %q accepted", route)
		}
	}
}
