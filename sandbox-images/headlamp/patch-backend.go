// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

package main

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"
)

const upstreamStaticCopy = `// copyStaticFiles copies static files to a temporary directory.
// This is needed because squashFS is read-only (AppImage).
func copyStaticFiles(config *HeadlampConfig) error {
	dir, err := os.MkdirTemp(os.TempDir(), ".headlamp")
	if err != nil {
		logger.Log(logger.LevelError, nil, err, "Failed to create static dir")

		return err
	}

	err = os.CopyFS(dir, os.DirFS(config.StaticDir))
	if err != nil {
		logger.Log(logger.LevelError, nil, err, "Failed to copy files from static dir")

		return err
	}

	config.StaticDir = dir

	return nil
}`

const packagedStaticCopy = `// copyStaticFiles prepares the packaged server's restart-bounded writable frontend.
func copyStaticFiles(config *HeadlampConfig) error {
	dir, err := prepareStaticCache(config.StaticDir, os.TempDir(), os.Getenv("HEADLAMP_STATIC_CACHE_NAME"))
	if err != nil {
		logger.Log(logger.LevelError, nil, err, "Failed to prepare private static cache")

		return err
	}

	config.StaticDir = dir

	return nil
}`

var oidcNavigationPatches = [][2]string{
	{"\tCluster      string // cluster context name this is associated with\n", "\tCluster      string // cluster context name this is associated with\n\tReturnTo     string // optional cluster-relative top-level navigation\n"},
	{"\t\t\tCluster:  cluster,\n", "\t\t\tCluster:  cluster,\n\t\t\tReturnTo: oidcTopLevelReturn(r.URL.Query()),\n"},
	{`redirectURL += fmt.Sprintf("auth?cluster=%1s", oauthConfig.Cluster)`, `redirectURL += oidcCallbackPath(oauthConfig.Cluster, oauthConfig.ReturnTo)`},
}

func patchOidcNavigation(source []byte) ([]byte, error) {
	result := string(source)
	for _, patch := range oidcNavigationPatches {
		if strings.Count(result, patch[0]) != 1 {
			return nil, fmt.Errorf("pinned Headlamp OIDC navigation changed; refusing to patch")
		}
		result = strings.Replace(result, patch[0], patch[1], 1)
	}
	return []byte(result), nil
}

func patchBackend(source []byte) ([]byte, error) {
	if strings.Count(string(source), upstreamStaticCopy) != 1 {
		return nil, fmt.Errorf("pinned Headlamp static-copy implementation changed; refusing to patch")
	}
	return []byte(strings.Replace(string(source), upstreamStaticCopy, packagedStaticCopy, 1)), nil
}

func main() {
	if len(os.Args) != 2 {
		panic("usage: patch-backend <extracted backend directory>")
	}
	file := filepath.Join(os.Args[1], "cmd", "headlamp.go")
	source, err := os.ReadFile(file)
	if err == nil {
		source, err = patchBackend(source)
	}
	if err == nil {
		source, err = patchOidcNavigation(source)
	}
	if err == nil {
		err = os.WriteFile(file, source, 0o644)
	}
	if err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
}
