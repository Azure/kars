// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

package saml

import (
	"crypto/x509"
	"os"
	"strings"
	"testing"
	"time"

	dsig "github.com/russellhaering/goxmldsig"
)

func TestKarsSAMLFixtureCertificateValidity(t *testing.T) {
	cert, err := loadCert("testdata/oam-ca.pem")
	if err != nil {
		t.Fatal(err)
	}
	data, err := os.ReadFile("testdata/oam-resp.xml")
	if err != nil {
		t.Fatal(err)
	}
	for _, tc := range []struct {
		name  string
		now   time.Time
		valid bool
	}{
		{"at signed fixture time", time.Date(2016, time.December, 12, 16, 54, 35, 0, time.UTC), true},
		{"before certificate validity", cert.NotBefore.Add(-time.Second), false},
		{"after certificate expiry", cert.NotAfter.Add(time.Second), false},
	} {
		t.Run(tc.name, func(t *testing.T) {
			validator := dsig.NewDefaultValidationContext(certStore{[]*x509.Certificate{cert}})
			if validator.Clock != nil {
				t.Fatal("production validation context must default to the real clock")
			}
			validator.Clock = dsig.NewFakeClockAt(tc.now)
			_, rootVerified, err := verifyResponseSig(validator, data)
			if tc.valid {
				if err != nil || rootVerified {
					t.Fatalf("expected verified assertion with unsigned root: rootVerified=%v, error=%v", rootVerified, err)
				}
			} else if err == nil || !strings.Contains(err.Error(), "Cert is not valid at this time") {
				t.Fatalf("certificate validity must remain enforced, got %v", err)
			}
		})
	}
}
