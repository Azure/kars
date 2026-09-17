// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// Collect notices from actual linked modules, not from the entire module cache.
package main

import (
	"encoding/json"
	"fmt"
	"io"
	"io/fs"
	"os"
	"path/filepath"
	"runtime"
	"sort"
	"strings"
)

type module struct {
	Path, Version, Dir string
	Main               bool
	Replace            *module
}

func runtimeModules(input io.Reader) (map[string]module, error) {
	decoder := json.NewDecoder(input)
	modules := map[string]module{
		"golang.org/toolchain@" + runtime.Version(): {Dir: runtime.GOROOT()},
	}
	foundDex := false
	for {
		var pkg struct {
			ImportPath string
			Module     *module
		}
		if err := decoder.Decode(&pkg); err != nil {
			if err == io.EOF {
				break
			}
			return nil, err
		}
		if pkg.ImportPath == "" {
			return nil, fmt.Errorf("runtime package inventory contains a missing import path")
		}
		if pkg.ImportPath == "golang.org/x/crypto/openpgp" ||
			strings.HasPrefix(pkg.ImportPath, "golang.org/x/crypto/openpgp/") {
			return nil, fmt.Errorf("forbidden compiled runtime package: %s (GO-2026-5932)", pkg.ImportPath)
		}
		if pkg.ImportPath == "github.com/dexidp/dex/cmd/dex" {
			foundDex = true
		}
		if pkg.Module == nil || pkg.Module.Main {
			continue
		}
		m := *pkg.Module
		if m.Replace != nil {
			m.Dir = m.Replace.Dir
		}
		modules[m.Path+"@"+m.Version] = m
	}
	if !foundDex {
		return nil, fmt.Errorf("runtime package inventory does not include the Dex command")
	}
	return modules, nil
}

func run() error {
	if len(os.Args) != 3 {
		return fmt.Errorf("usage: notices packages.json output-directory")
	}
	f, err := os.Open(os.Args[1])
	if err != nil {
		return err
	}
	defer f.Close()
	modules, err := runtimeModules(f)
	if err != nil {
		return err
	}
	keys := make([]string, 0, len(modules))
	for key := range modules {
		keys = append(keys, key)
	}
	sort.Strings(keys)
	for _, key := range keys {
		m := modules[key]
		rootLicense := false
		err := filepath.WalkDir(m.Dir, func(path string, entry fs.DirEntry, err error) error {
			if err != nil {
				return err
			}
			name := strings.ToUpper(entry.Name())
			if entry.IsDir() {
				return nil
			}
			if !strings.HasPrefix(name, "LICENSE") && !strings.HasPrefix(name, "LICENCE") && !strings.HasPrefix(name, "COPYING") &&
				!strings.HasPrefix(name, "NOTICE") && !strings.HasPrefix(name, "COPYRIGHT") &&
				name != "AUTHORS" && name != "PATENTS" {
				return nil
			}
			rel, err := filepath.Rel(m.Dir, path)
			if err != nil {
				return err
			}
			if filepath.Dir(rel) == "." && (strings.HasPrefix(name, "LICENSE") || strings.HasPrefix(name, "LICENCE") || strings.HasPrefix(name, "COPYING")) {
				rootLicense = true
			}
			dest := filepath.Join(os.Args[2], key, rel)
			if err := os.MkdirAll(filepath.Dir(dest), 0755); err != nil {
				return err
			}
			data, err := os.ReadFile(path)
			if err != nil {
				return err
			}
			return os.WriteFile(dest, data, 0644)
		})
		if err != nil {
			return fmt.Errorf("%s: %w", key, err)
		}
		// Dex's nested API is licensed by the upstream repository root.
		if !rootLicense && m.Path == "github.com/dexidp/dex/api/v2" {
			data, err := os.ReadFile("/src/dex/LICENSE")
			if err != nil {
				return err
			}
			dest := filepath.Join(os.Args[2], key)
			if err := os.MkdirAll(dest, 0755); err != nil {
				return err
			}
			if err := os.WriteFile(filepath.Join(dest, "LICENSE"), data, 0644); err != nil {
				return err
			}
			rootLicense = true
		}
		if !rootLicense {
			return fmt.Errorf("%s: missing root license; review upstream attribution before shipping", key)
		}
	}
	return os.WriteFile(filepath.Join(os.Args[2], "MODULES"), []byte(strings.Join(keys, "\n")+"\n"), 0644)
}

func main() {
	if err := run(); err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
}
