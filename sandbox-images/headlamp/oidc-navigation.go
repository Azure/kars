// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

package main

import (
	"net/url"
	"regexp"
)

var oidcReturnRoute = regexp.MustCompile(`^/([A-Za-z0-9_-]+/)*[A-Za-z0-9_-]*$`)

// Return routes are cluster-relative, never external URLs or arbitrary redirects.
func oidcCallbackPath(cluster, returnTo string) string {
	if cluster != "" && cluster != "." && cluster != ".." && oidcReturnRoute.MatchString(returnTo) {
		return "c/" + url.PathEscape(cluster) + returnTo
	}
	return "auth?cluster=" + url.QueryEscape(cluster)
}

func oidcTopLevelReturn(query url.Values) string {
	value := query.Get("return")
	if !oidcReturnRoute.MatchString(value) {
		return ""
	}
	return value
}
