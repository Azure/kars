// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

package main

import (
	"go/parser"
	"go/token"
	"os"
	"path/filepath"
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
	tracingFile := filepath.Join(filepath.Dir(file), "..", "pkg", "telemetry", "tracing.go")
	tracing, err := os.ReadFile(tracingFile)
	if err != nil {
		t.Fatal(err)
	}
	tracing, err = patchTracing(tracing)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := parser.ParseFile(token.NewFileSet(), tracingFile, tracing, parser.AllErrors); err != nil {
		t.Fatal(err)
	}
}

func TestPatchTracingPreservesServiceSpanName(t *testing.T) {
	original := []byte(`package telemetry
func TracingMiddleware(serviceName string) func(http.Handler) http.Handler {
	return func(next http.Handler) http.Handler {
		return otelhttp.NewHandler(next, serviceName,
			otelhttp.WithMessageEvents(otelhttp.ReadEvents, otelhttp.WriteEvents),
		)
	}
}
`)
	patched, err := patchTracing(original)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := parser.ParseFile(token.NewFileSet(), "tracing.go", patched, parser.AllErrors); err != nil {
		t.Fatal(err)
	}
	addition := `			otelhttp.WithSpanNameFormatter(func(operation string, _ *http.Request) string {
				return operation
			}),
`
	if strings.Replace(string(patched), addition, "", 1) != string(original) {
		t.Fatal("tracing patch changed bytes beyond the span-name formatter")
	}
	for _, invalid := range [][]byte{
		patched,
		[]byte(strings.Replace(string(original), "serviceName,", "changedName,", 1)),
		append(append([]byte{}, original...), original...),
	} {
		if _, err := patchTracing(invalid); err == nil {
			t.Fatal("tracing patch must reject already-patched, drifted or duplicate source")
		}
	}
}
