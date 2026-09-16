// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

package main

import (
	"encoding/json"
	"strings"
	"testing"
)

func packageInventory(paths ...string) string {
	var inventory strings.Builder
	encoder := json.NewEncoder(&inventory)
	for _, path := range paths {
		if err := encoder.Encode(map[string]string{"ImportPath": path}); err != nil {
			panic(err)
		}
	}
	return inventory.String()
}

func TestRuntimeModulesRejectsOpenPGPAndEverySubpackage(t *testing.T) {
	for _, suffix := range []string{"", "/packet", "/armor", "/clearsign", "/errors", "/elgamal", "/s2k", "/future/nested"} {
		t.Run(suffix, func(t *testing.T) {
			path := "golang.org/x/crypto/openpgp" + suffix
			_, err := runtimeModules(strings.NewReader(packageInventory("github.com/dexidp/dex/cmd/dex", path)))
			if err == nil || !strings.Contains(err.Error(), "forbidden compiled runtime package: "+path) {
				t.Fatalf("expected fatal OpenPGP package rejection, got %v", err)
			}
		})
	}
}

func TestRuntimeModulesKeepsOtherCryptoAndModuleEvidence(t *testing.T) {
	inventory := packageInventory(
		"crypto/rsa", "github.com/dexidp/dex/cmd/dex", "golang.org/x/crypto/bcrypt",
		"golang.org/x/crypto/openpgpcompat", "github.com/ProtonMail/go-crypto/openpgp",
	) + `{"ImportPath":"github.com/Azure/go-ntlmssp","Module":{"Path":"github.com/Azure/go-ntlmssp","Version":"v0.1.1","Dir":"/verified/module"}}`
	modules, err := runtimeModules(strings.NewReader(inventory))
	if err != nil {
		t.Fatal(err)
	}
	if modules["github.com/Azure/go-ntlmssp@v0.1.1"].Dir != "/verified/module" {
		t.Fatal("actual selected module evidence was not retained")
	}
}

func TestRuntimeModulesRejectsEmptyMalformedOrWrongTargetInventory(t *testing.T) {
	for _, input := range []string{"", "{", "{}", "null", packageInventory("golang.org/x/crypto/bcrypt")} {
		if _, err := runtimeModules(strings.NewReader(input)); err == nil {
			t.Fatalf("invalid runtime inventory accepted: %q", input)
		}
	}
}
