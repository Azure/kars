// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

package main

import (
	"go/parser"
	"go/token"
	"os"
	"strings"
	"testing"
)

func TestPatchBackend(t *testing.T) {
	original := []byte("package main\n\n" + upstreamStaticCopy + "\n// untouched\n")
	patched, err := patchBackend(original)
	if err != nil {
		t.Fatal(err)
	}
	if string(patched) != "package main\n\n"+packagedStaticCopy+"\n// untouched\n" {
		t.Fatal("patch changed bytes outside the static-copy function")
	}
	if _, err := parser.ParseFile(token.NewFileSet(), "headlamp.go", patched, 0); err != nil {
		t.Fatal(err)
	}
	for _, invalid := range [][]byte{
		patched,
		[]byte(strings.Replace(string(original), "os.CopyFS", "changedCopyFS", 1)),
		append(append([]byte{}, original...), original...),
	} {
		if _, err := patchBackend(invalid); err == nil {
			t.Fatal("patch must reject already-patched, drifted or duplicate source")
		}
	}
}

func TestPatchOidcNavigation(t *testing.T) {
	var original, expected strings.Builder
	for _, patch := range oidcNavigationPatches {
		original.WriteString(patch[0] + "\n// untouched\n")
		expected.WriteString(patch[1] + "\n// untouched\n")
	}
	patched, err := patchOidcNavigation([]byte(original.String()))
	if err != nil || string(patched) != expected.String() {
		t.Fatalf("unexpected navigation patch: %v", err)
	}
	for _, invalid := range [][]byte{
		patched,
		[]byte(strings.Replace(original.String(), "Cluster:  cluster", "Cluster: changed", 1)),
		[]byte(original.String() + original.String()),
	} {
		if _, err := patchOidcNavigation(invalid); err == nil {
			t.Fatal("navigation patch must reject already-patched, drifted or duplicate source")
		}
	}
}

func TestPinnedBackendSource(t *testing.T) {
	file := os.Getenv("HEADLAMP_UPSTREAM_SOURCE")
	if file == "" {
		t.Skip("set HEADLAMP_UPSTREAM_SOURCE to the pinned backend/cmd/headlamp.go")
	}
	original, err := os.ReadFile(file)
	if err != nil {
		t.Fatal(err)
	}
	patched, err := patchBackend(original)
	if err != nil {
		t.Fatal(err)
	}
	patched, err = patchOidcNavigation(patched)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := parser.ParseFile(token.NewFileSet(), file, patched, parser.AllErrors); err != nil {
		t.Fatal(err)
	}
	if strings.Contains(string(patched), `os.MkdirTemp(os.TempDir(), ".headlamp")`) {
		t.Fatal("unbounded static-copy path remains")
	}
}
