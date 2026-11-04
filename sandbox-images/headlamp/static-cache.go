// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

package main

import (
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"syscall"
)

// prepareStaticCache replaces only this packaged server's writable frontend copy.
// Restart recovery happens before copying, so even SIGKILL leaves at most one tree.
func prepareStaticCache(source, tempDir, name string) (string, error) {
	if name == "" || name == "." || name == ".." || filepath.Base(name) != name {
		return "", fmt.Errorf("HEADLAMP_STATIC_CACHE_NAME must be a single directory name")
	}
	tempDir, err := filepath.EvalSymlinks(tempDir)
	if err != nil {
		return "", err
	}
	tempDir, err = filepath.Abs(tempDir)
	if err != nil {
		return "", err
	}
	source, err = filepath.EvalSymlinks(source)
	if err != nil {
		return "", err
	}
	source, err = filepath.Abs(source)
	if err != nil {
		return "", err
	}
	privateDir := filepath.Join(tempDir, name)
	if cachePathsOverlap(source, privateDir) {
		return "", fmt.Errorf("static source and private cache must not overlap")
	}

	if err := os.Mkdir(privateDir, 0o700); err != nil && !errors.Is(err, os.ErrExist) {
		return "", err
	}
	info, err := os.Lstat(privateDir)
	if err != nil {
		return "", err
	}
	owner, ok := info.Sys().(*syscall.Stat_t)
	if !info.IsDir() || info.Mode().Perm() != 0o700 || !ok || owner.Uid != uint32(os.Geteuid()) {
		return "", fmt.Errorf("static cache must be a private directory owned by the server user: %s", privateDir)
	}

	root, err := os.OpenRoot(privateDir)
	if err != nil {
		return "", err
	}
	defer root.Close()
	opened, err := root.Stat(".")
	if err != nil {
		return "", err
	}
	if !os.SameFile(info, opened) {
		return "", fmt.Errorf("static cache directory changed during startup")
	}

	// The private parent is inaccessible to other users. Root confines deletion;
	// RemoveAll unlinks stale symlinks, never their targets.
	destination := filepath.Join(privateDir, "frontend")
	if err := root.RemoveAll("frontend"); err != nil {
		return "", err
	}
	if err := os.CopyFS(destination, os.DirFS(source)); err != nil {
		return "", errors.Join(err, root.RemoveAll("frontend"))
	}
	return destination, nil
}

func cachePathsOverlap(a, b string) bool {
	within := func(parent, child string) bool {
		rel, err := filepath.Rel(parent, child)
		return err == nil && rel != ".." && !strings.HasPrefix(rel, ".."+string(filepath.Separator))
	}
	return within(a, b) || within(b, a)
}
