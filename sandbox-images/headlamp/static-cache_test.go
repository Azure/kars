// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

package main

import (
	"os"
	"path/filepath"
	"syscall"
	"testing"
)

func cacheFixture(t *testing.T) (source, tempDir string) {
	t.Helper()
	root := t.TempDir()
	source, tempDir = filepath.Join(root, "immutable"), filepath.Join(root, "runtime")
	for _, dir := range []string{source, tempDir} {
		if err := os.Mkdir(dir, 0o700); err != nil {
			t.Fatal(err)
		}
	}
	writeCacheFixture(t, filepath.Join(source, "index.html"), "original frontend")
	return source, tempDir
}

func writeCacheFixture(t *testing.T, file, value string) {
	t.Helper()
	if err := os.WriteFile(file, []byte(value), 0o600); err != nil {
		t.Fatal(err)
	}
}

func assertCacheFixture(t *testing.T, file, expected string) {
	t.Helper()
	value, err := os.ReadFile(file)
	if err != nil || string(value) != expected {
		t.Fatalf("%s: got %q, %v; want %q", file, value, err, expected)
	}
}

func TestStaticCacheRepeatedStartup(t *testing.T) {
	source, tempDir := cacheFixture(t)
	unrelated := filepath.Join(tempDir, ".headlamp-unrelated")
	writeCacheFixture(t, unrelated, "not ours")
	privateDir := filepath.Join(tempDir, "packaged-server")
	for attempt := 0; attempt < 8; attempt++ {
		cache, err := prepareStaticCache(source, tempDir, "packaged-server")
		if err != nil {
			t.Fatal(err)
		}
		if cache != filepath.Join(privateDir, "frontend") {
			t.Fatalf("cache changed location: %s", cache)
		}
		assertCacheFixture(t, filepath.Join(cache, "index.html"), "original frontend")
		entries, err := os.ReadDir(cache)
		if err != nil || len(entries) != 1 {
			t.Fatalf("stale or partial files survived: %v, %v", entries, err)
		}
		// No shutdown cleanup: model a killed process after runtime rewriting.
		writeCacheFixture(t, filepath.Join(cache, "index.html"), "runtime rewrite")
		writeCacheFixture(t, filepath.Join(cache, "stale.js"), "previous version")
	}
	entries, err := os.ReadDir(privateDir)
	if err != nil || len(entries) != 1 || entries[0].Name() != "frontend" {
		t.Fatalf("frontend copies accumulated: %v, %v", entries, err)
	}
	assertCacheFixture(t, filepath.Join(source, "index.html"), "original frontend")
	assertCacheFixture(t, unrelated, "not ours")
}

func TestStaticCacheIncompleteCopy(t *testing.T) {
	source, tempDir := cacheFixture(t)
	privateDir := filepath.Join(tempDir, "packaged-server")
	if err := os.MkdirAll(filepath.Join(privateDir, "frontend", "assets"), 0o700); err != nil {
		t.Fatal(err)
	}
	writeCacheFixture(t, filepath.Join(privateDir, "frontend", "assets", "partial"), "interrupted")
	writeCacheFixture(t, filepath.Join(privateDir, "unrelated"), "keep")
	cache, err := prepareStaticCache(source, tempDir, "packaged-server")
	if err != nil {
		t.Fatal(err)
	}
	if _, err := os.Stat(filepath.Join(cache, "assets")); !os.IsNotExist(err) {
		t.Fatalf("partial copy not cleared: %v", err)
	}
	assertCacheFixture(t, filepath.Join(privateDir, "unrelated"), "keep")
}

func TestStaticCacheSymlinkSafety(t *testing.T) {
	for _, position := range []string{"private-root", "frontend", "nested"} {
		t.Run(position, func(t *testing.T) {
			source, tempDir := cacheFixture(t)
			privateDir := filepath.Join(tempDir, "packaged-server")
			link := privateDir
			if position != "private-root" {
				if err := os.Mkdir(privateDir, 0o700); err != nil {
					t.Fatal(err)
				}
				link = filepath.Join(privateDir, "frontend")
			}
			if position == "nested" {
				if err := os.Mkdir(link, 0o700); err != nil {
					t.Fatal(err)
				}
				link = filepath.Join(link, "escape")
			}
			if err := os.Symlink(source, link); err != nil {
				t.Fatal(err)
			}
			_, err := prepareStaticCache(source, tempDir, "packaged-server")
			if (position == "private-root") != (err != nil) {
				t.Fatalf("unexpected result for %s: %v", position, err)
			}
			assertCacheFixture(t, filepath.Join(source, "index.html"), "original frontend")
		})
	}
}

func TestStaticCacheCopyFailure(t *testing.T) {
	source, tempDir := cacheFixture(t)
	// WalkDir copies index.html first, then fails on this unsupported source FIFO.
	fifo := filepath.Join(source, "z-unsupported-fifo")
	if err := syscall.Mkfifo(fifo, 0o600); err != nil {
		t.Fatal(err)
	}
	cache, err := prepareStaticCache(source, tempDir, "packaged-server")
	if err == nil || cache != "" {
		t.Fatalf("copy failure returned usable cache: %q, %v", cache, err)
	}
	if _, err := os.Lstat(filepath.Join(tempDir, "packaged-server", "frontend")); !os.IsNotExist(err) {
		t.Fatalf("failed copy left partial tree: %v", err)
	}
	assertCacheFixture(t, filepath.Join(source, "index.html"), "original frontend")
	if err := os.Remove(fifo); err != nil {
		t.Fatal(err)
	}
	if _, err := prepareStaticCache(source, tempDir, "packaged-server"); err != nil {
		t.Fatalf("could not recover after copy failure: %v", err)
	}
}

func TestStaticCacheRejectsUnsafeConfiguration(t *testing.T) {
	source, tempDir := cacheFixture(t)
	for _, name := range []string{"", ".", "..", "../immutable", "/absolute", "a/b"} {
		if _, err := prepareStaticCache(source, tempDir, name); err == nil {
			t.Fatalf("accepted unsafe name %q", name)
		}
	}
	privateDir := filepath.Join(tempDir, "packaged-server")
	if err := os.Mkdir(privateDir, 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.Chmod(privateDir, 0o755); err != nil {
		t.Fatal(err)
	}
	if _, err := prepareStaticCache(source, tempDir, "packaged-server"); err == nil {
		t.Fatal("accepted a cache writable/readable by other users")
	}
	assertCacheFixture(t, filepath.Join(source, "index.html"), "original frontend")
}

func TestStaticCacheReadOnlySource(t *testing.T) {
	source, tempDir := cacheFixture(t)
	file := filepath.Join(source, "index.html")
	if err := os.Chmod(file, 0o444); err != nil {
		t.Fatal(err)
	}
	if err := os.Chmod(source, 0o555); err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		if err := os.Chmod(source, 0o700); err != nil {
			t.Error(err)
		}
	})
	for attempt := 0; attempt < 2; attempt++ {
		cache, err := prepareStaticCache(source, tempDir, "packaged-server")
		if err != nil {
			t.Fatal(err)
		}
		writeCacheFixture(t, filepath.Join(cache, "index.html"), "rewritten")
	}
	assertCacheFixture(t, file, "original frontend")
	info, err := os.Stat(file)
	if err != nil || info.Mode().Perm() != 0o444 {
		t.Fatalf("source permissions changed: %v, %v", info, err)
	}
}

func TestStaticCacheRejectsSourceOverlap(t *testing.T) {
	for _, position := range []string{"ancestor", "equal", "descendant", "source-alias"} {
		t.Run(position, func(t *testing.T) {
			_, tempDir := cacheFixture(t)
			privateDir := filepath.Join(tempDir, "packaged-server")
			source := privateDir
			switch position {
			case "ancestor":
				source = tempDir
			case "descendant", "source-alias":
				source = filepath.Join(privateDir, "frontend")
			}
			if err := os.MkdirAll(source, 0o700); err != nil {
				t.Fatal(err)
			}
			writeCacheFixture(t, filepath.Join(source, "index.html"), "preserve")
			if position == "source-alias" {
				alias := filepath.Join(tempDir, "alias")
				if err := os.Symlink(source, alias); err != nil {
					t.Fatal(err)
				}
				source = alias
			}
			if _, err := prepareStaticCache(source, tempDir, "packaged-server"); err == nil {
				t.Fatal("accepted source/cache overlap")
			}
			assertCacheFixture(t, filepath.Join(source, "index.html"), "preserve")
		})
	}
}
