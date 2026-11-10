//go:build cgo

// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

package grants

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"log/slog"
	"net/url"
	"os"
	"os/exec"
	"sync/atomic"
	"testing"
	"time"

	"github.com/go-jose/go-jose/v4"
	"github.com/stretchr/testify/require"

	"github.com/dexidp/dex/connector"
	"github.com/dexidp/dex/server/connectors"
	"github.com/dexidp/dex/server/internal"
	"github.com/dexidp/dex/server/oauth2"
	"github.com/dexidp/dex/server/signer"
	"github.com/dexidp/dex/server/tokens"
	"github.com/dexidp/dex/storage"
	"github.com/dexidp/dex/storage/memory"
	dexsql "github.com/dexidp/dex/storage/sql"
)

// Memory's mutex ignores context cancellation. Isolate each backend in a child
// test process so a regression cannot strand a worker or block storage cleanup.
func TestKarsRefreshStorageDeadlock(t *testing.T) {
	backend := os.Getenv("KARS_DEX_REFRESH_TEST_BACKEND")
	if backend == "" {
		for _, backend := range []string{"memory", "sqlite"} {
			t.Run(backend, func(t *testing.T) {
				ctx, cancel := context.WithTimeout(t.Context(), 30*time.Second)
				defer cancel()
				cmd := exec.CommandContext(ctx, os.Args[0], "-test.run=^TestKarsRefreshStorageDeadlock$", "-test.v", "-test.timeout=25s")
				cmd.Env = append(os.Environ(), "KARS_DEX_REFRESH_TEST_BACKEND="+backend)
				cmd.WaitDelay = 2 * time.Second
				output, err := cmd.CombinedOutput()
				require.NoError(t, ctx.Err(), "refresh worker exceeded hard deadline:\n%s", output)
				require.NoError(t, err, "refresh worker failed:\n%s", output)
				t.Logf("%s", output)
			})
		}
		return
	}
	require.Contains(t, []string{"memory", "sqlite"}, backend)
	sig, err := signer.NewMockSigner(nil)
	require.NoError(t, err)

	for _, legacy := range []bool{false, true} {
		t.Run(fmt.Sprintf("dynamic_password/legacy=%t", legacy), func(t *testing.T) {
			f := newKarsRefreshFixture(t, backend, sig)
			if legacy {
				require.NoError(t, f.store.UpdateRefreshToken(t.Context(), f.id.RefreshId, func(old storage.RefreshToken) (storage.RefreshToken, error) {
					old.ConnectorData = []byte("legacy-data")
					return old, nil
				}))
			}
			f.advance(2 * time.Minute)
			result := f.authorize(t, f.lookup(t), f.password())
			require.NotEqual(t, f.raw, result.RefreshToken)
			claims := f.claims(t, result)
			require.Equal(t, "Updated Name", claims.Name)
			require.Equal(t, "updated-user", claims.PreferredUsername)
			require.Equal(t, []string{"updated-group"}, claims.Groups)
			require.NotNil(t, claims.EmailVerified)
			require.False(t, *claims.EmailVerified)
			require.Equal(t, "member@example.com", claims.Email)
			require.Equal(t, "nonce", claims.Nonce)
			require.Equal(t, 3600, result.ExpiresIn)
			require.Equal(t, int32(1), f.store.passwordReads.Load())
			stored := f.stored(t)
			require.Equal(t, "user", stored.Claims.UserID)
			require.Equal(t, f.id.Token, stored.ObsoleteToken)
			require.Empty(t, stored.ConnectorData)
			require.Equal(t, f.now(), stored.LastUsed)
			offline, err := f.store.Storage.GetOfflineSessions(t.Context(), "user", "local")
			require.NoError(t, err)
			expectedData := "offline-data"
			if legacy {
				expectedData = "legacy-data"
			}
			require.Equal(t, expectedData, string(offline.ConnectorData))
			require.Equal(t, stored.LastUsed, offline.Refresh["client"].LastUsed)
		})
	}

	t.Run("rotation_disabled_still_refreshes_identity", func(t *testing.T) {
		f := newKarsRefreshFixture(t, backend, sig)
		f.grant.policy = tokens.NewRefreshStrategy(false, 0, 0, time.Minute, f.now)
		f.advance(2 * time.Minute)
		response := f.authorize(t, f.lookup(t), f.password())
		require.Equal(t, f.raw, response.RefreshToken)
		require.Equal(t, "Updated Name", f.claims(t, response).Name)
		require.Equal(t, f.now(), f.stored(t).LastUsed)
		require.Empty(t, f.stored(t).ObsoleteToken)
	})

	t.Run("connector_without_refresh_preserves_identity", func(t *testing.T) {
		f := newKarsRefreshFixture(t, backend, sig)
		f.advance(2 * time.Minute)
		response := f.authorize(t, f.lookup(t), connectors.Connector{Connector: struct{}{}})
		require.Equal(t, "Old Name", f.claims(t, response).Name)
		require.NotEqual(t, f.raw, response.RefreshToken)
		require.Zero(t, f.store.passwordReads.Load())
	})

	for _, local := range []bool{false, true} {
		t.Run(fmt.Sprintf("concurrent_once/local=%t", local), func(t *testing.T) {
			f := newKarsRefreshFixture(t, backend, sig)
			f.advance(2 * time.Minute)
			requests := []*Request{f.lookup(t), f.lookup(t)}
			f.store.readGate = make(chan struct{})
			var calls atomic.Int32
			conn := f.external(&calls, nil)
			if local {
				conn = f.password()
			}
			type outcome struct {
				response Responder
				err      error
			}
			results := make(chan outcome, len(requests))
			for _, request := range requests {
				go func() {
					response, err := f.grant.Authorize(t.Context(), request, f.client, conn)
					results <- outcome{response, err}
				}()
			}
			var raw string
			for range requests {
				result := <-results
				require.NoError(t, result.err)
				response, ok := result.response.(tokens.Response)
				require.True(t, ok)
				f.claims(t, response)
				if raw == "" {
					raw = response.RefreshToken
				}
				require.Equal(t, raw, response.RefreshToken)
			}
			f.store.readGate = nil
			require.NotEqual(t, f.raw, raw)
			if local {
				require.Equal(t, int32(2), f.store.passwordReads.Load(), "each request may prepare a read-only password result")
				require.Zero(t, calls.Load())
			} else {
				require.Equal(t, int32(1), calls.Load(), "external refresh must stay inside rotation's claim checks")
				offline, err := f.store.GetOfflineSessions(t.Context(), "user", "local")
				require.NoError(t, err)
				require.Equal(t, "new-offline-data", string(offline.ConnectorData))
			}
			require.Equal(t, f.id.Token, f.stored(t).ObsoleteToken)
		})
	}

	for _, failure := range []string{"offline", "password", "deleted", "reassigned", "external"} {
		for _, reuse := range []bool{false, true} {
			t.Run(fmt.Sprintf("deferred_errors/%s/reuse=%t", failure, reuse), func(t *testing.T) {
				f := newKarsRefreshFixture(t, backend, sig)
				conn := f.password()
				var calls atomic.Int32
				expected := "user not found"
				switch failure {
				case "offline":
					expected = "offline read failed"
					f.store.offlineErr = errors.New(expected)
				case "password":
					expected = "get password: password read failed"
					f.store.passwordErr = errors.New("password read failed")
				case "deleted":
					require.NoError(t, f.store.DeletePassword(t.Context(), "member@example.com"))
				case "reassigned":
					require.NoError(t, f.store.UpdatePassword(t.Context(), "member@example.com", func(p storage.Password) (storage.Password, error) {
						p.UserID = "different-user"
						return p, nil
					}))
				case "external":
					expected = "external refresh failed"
					conn = f.external(&calls, errors.New(expected))
				}
				if !reuse {
					f.advance(2 * time.Minute)
				}
				before := f.stored(t)
				response, err := f.grant.Authorize(t.Context(), f.lookup(t), f.client, conn)
				if reuse {
					require.NoError(t, err, "reuse must not consume prepared identity errors")
					require.Equal(t, f.raw, response.(tokens.Response).RefreshToken)
					require.Zero(t, calls.Load())
				} else {
					require.Nil(t, response)
					var oerr *oauth2.Error
					require.ErrorAs(t, err, &oerr)
					require.Equal(t, oauth2.InvalidRequest, oerr.Type)
					require.Contains(t, f.logs.String(), expected)
					if failure == "external" {
						require.Equal(t, int32(1), calls.Load())
					}
				}
				require.Equal(t, before, f.stored(t), "failed/reused refresh must not change claims or consume the token")
			})
		}
	}

	t.Run("stale_claim_and_scope_rejected_before_external_refresh", func(t *testing.T) {
		f := newKarsRefreshFixture(t, backend, sig)
		f.advance(2 * time.Minute)
		stale := f.lookup(t)
		var calls atomic.Int32
		conn := f.external(&calls, nil)
		f.authorize(t, f.lookup(t), conn)
		f.advance(2 * time.Minute)
		_, err := f.grant.Authorize(t.Context(), stale, f.client, conn)
		require.Error(t, err)
		require.Equal(t, int32(1), calls.Load())
		_, oerr := f.grant.ConnectorID(t.Context(), &Request{RefreshToken: f.raw}, f.client)
		require.NotNil(t, oerr)
		require.Equal(t, oauth2.InvalidGrant, oerr.Type)
		stale.Scopes = []string{"not-authorized"}
		_, err = f.grant.Authorize(t.Context(), stale, f.client, conn)
		require.Error(t, err)
		require.Equal(t, int32(1), calls.Load())
	})

	for _, rejection := range []string{"wrong_client", "invalid_secret", "absolute_expiry", "idle_expiry"} {
		t.Run(rejection, func(t *testing.T) {
			f := newKarsRefreshFixture(t, backend, sig)
			req := &Request{RefreshToken: f.raw}
			client := f.client
			switch rejection {
			case "wrong_client":
				client.ID = "other-client"
			case "invalid_secret":
				var err error
				req.RefreshToken, err = internal.Marshal(&internal.RefreshToken{RefreshId: f.id.RefreshId, Token: "incorrect"})
				require.NoError(t, err)
			case "absolute_expiry":
				f.grant.policy = tokens.NewRefreshStrategy(true, time.Minute, 0, time.Minute, f.now)
				f.advance(2 * time.Minute)
			case "idle_expiry":
				f.grant.policy = tokens.NewRefreshStrategy(true, 0, time.Minute, time.Minute, f.now)
				f.advance(2 * time.Minute)
			}
			_, oerr := f.grant.ConnectorID(t.Context(), req, client)
			require.NotNil(t, oerr)
			require.Equal(t, oauth2.InvalidGrant, oerr.Type)
			require.Zero(t, f.store.passwordReads.Load())
		})
	}

	t.Run("sessions_use_cached_identity_without_password_refresh", func(t *testing.T) {
		f := newKarsRefreshFixture(t, backend, sig)
		f.grant.sessionsEnabled = true
		login := f.now().Add(-time.Hour)
		require.NoError(t, f.store.CreateUserIdentity(t.Context(), storage.UserIdentity{
			UserID: "user", ConnectorID: "local", LastLogin: login,
			Claims: storage.Claims{UserID: "user", Username: "Cached Name", Email: "cached@example.com"},
		}))
		f.store.passwordErr = errors.New("sessions must not read passwords")
		f.advance(2 * time.Minute)
		response := f.authorize(t, f.lookup(t), f.password())
		claims := f.claims(t, response)
		require.Equal(t, "Cached Name", claims.Name)
		require.Equal(t, login.Unix(), claims.AuthTime)
		require.Zero(t, f.store.passwordReads.Load())
	})
}

type karsRefreshStorage struct {
	storage.Storage
	inside        atomic.Int32
	passwordReads atomic.Int32
	offlineErr    error
	passwordErr   error
	readGate      chan struct{}
	gateReads     atomic.Int32
}

func (s *karsRefreshStorage) GetOfflineSessions(ctx context.Context, user, conn string) (storage.OfflineSessions, error) {
	if s.offlineErr != nil {
		return storage.OfflineSessions{}, s.offlineErr
	}
	result, err := s.Storage.GetOfflineSessions(ctx, user, conn)
	if s.readGate != nil {
		if s.gateReads.Add(1) == 2 {
			close(s.readGate)
		}
		<-s.readGate
	}
	return result, err
}

func (s *karsRefreshStorage) GetPassword(ctx context.Context, email string) (storage.Password, error) {
	s.passwordReads.Add(1)
	if s.passwordErr != nil {
		return storage.Password{}, s.passwordErr
	}
	return s.Storage.GetPassword(ctx, email)
}

func (s *karsRefreshStorage) UpdateRefreshToken(ctx context.Context, id string, updater func(storage.RefreshToken) (storage.RefreshToken, error)) error {
	return s.Storage.UpdateRefreshToken(ctx, id, func(old storage.RefreshToken) (storage.RefreshToken, error) {
		s.inside.Add(1)
		defer s.inside.Add(-1)
		return updater(old)
	})
}

type karsRefreshConnector func(context.Context, connector.Scopes, connector.Identity) (connector.Identity, error)

func (f karsRefreshConnector) Refresh(ctx context.Context, scopes connector.Scopes, ident connector.Identity) (connector.Identity, error) {
	return f(ctx, scopes, ident)
}

type karsRefreshFixture struct {
	grant  *refresh
	store  *karsRefreshStorage
	client storage.Client
	raw    string
	id     internal.RefreshToken
	clock  atomic.Int64
	sig    signer.Signer
	logs   bytes.Buffer
}

func newKarsRefreshFixture(t *testing.T, backend string, sig signer.Signer) *karsRefreshFixture {
	t.Helper()
	f := &karsRefreshFixture{client: storage.Client{ID: "client"}, sig: sig}
	f.clock.Store(time.Date(2026, 10, 28, 12, 0, 0, 0, time.UTC).UnixNano())
	logger := slog.New(slog.NewTextHandler(&f.logs, nil))
	var store storage.Storage
	if backend == "sqlite" {
		var err error
		store, err = (&dexsql.SQLite3{File: ":memory:"}).Open(logger)
		require.NoError(t, err)
	} else {
		store = memory.New(logger)
	}
	t.Cleanup(func() { require.NoError(t, store.Close()) })
	f.store = &karsRefreshStorage{Storage: store}
	issuer := tokens.NewIssuer(f.store, sig, url.URL{Scheme: "https", Host: "dex.example.com"}, time.Hour, f.now, logger)
	f.grant = &refresh{storage: f.store, issuer: issuer, policy: tokens.NewRefreshStrategy(true, 0, 0, time.Minute, f.now), now: f.now, logger: logger}
	verified := false
	require.NoError(t, store.CreatePassword(t.Context(), storage.Password{
		Email: "member@example.com", UserID: "user", Name: "Updated Name", PreferredUsername: "updated-user",
		EmailVerified: &verified, Groups: []string{"updated-group"}, Hash: []byte("unused-by-refresh"),
	}))
	var err error
	f.raw, err = issuer.Refresh.Create(t.Context(), tokens.Authorization{
		Client: f.client, ConnectorID: "local", Nonce: "nonce",
		Claims: storage.Claims{UserID: "user", Username: "Old Name", Email: "member@example.com", EmailVerified: true},
		Scopes: []string{"openid", "email", "profile", "groups", "offline_access"}, ConnectorData: []byte("offline-data"),
	})
	require.NoError(t, err)
	require.NoError(t, internal.Unmarshal(f.raw, &f.id))
	// Modern tokens carry data only on the offline session. This forces the
	// nested GetOfflineSessions path even on the very first fresh rotation.
	require.NoError(t, store.UpdateRefreshToken(t.Context(), f.id.RefreshId, func(old storage.RefreshToken) (storage.RefreshToken, error) {
		old.ConnectorData = nil
		return old, nil
	}))
	return f
}

func (f *karsRefreshFixture) now() time.Time          { return time.Unix(0, f.clock.Load()).UTC() }
func (f *karsRefreshFixture) advance(d time.Duration) { f.clock.Add(int64(d)) }

func (f *karsRefreshFixture) password() connectors.Connector {
	return connectors.Connector{Connector: connectors.NewPasswordDB(f.store)}
}

func (f *karsRefreshFixture) external(calls *atomic.Int32, failure error) connectors.Connector {
	return connectors.Connector{Connector: karsRefreshConnector(func(_ context.Context, scopes connector.Scopes, ident connector.Identity) (connector.Identity, error) {
		calls.Add(1)
		if f.store.inside.Load() != 1 {
			return ident, errors.New("external refresh escaped the storage updater")
		}
		if !scopes.OfflineAccess || !scopes.Groups || string(ident.ConnectorData) != "offline-data" {
			return ident, errors.New("connector scopes or offline data were lost")
		}
		ident.Username = "External Name"
		ident.ConnectorData = []byte("new-offline-data")
		return ident, failure
	})}
}

func (f *karsRefreshFixture) lookup(t *testing.T) *Request {
	t.Helper()
	req := &Request{RefreshToken: f.raw}
	conn, oerr := f.grant.ConnectorID(t.Context(), req, f.client)
	require.Nil(t, oerr)
	require.Equal(t, "local", conn)
	return req
}

func (f *karsRefreshFixture) stored(t *testing.T) storage.RefreshToken {
	t.Helper()
	stored, err := f.store.GetRefresh(t.Context(), f.id.RefreshId)
	require.NoError(t, err)
	return stored
}

func (f *karsRefreshFixture) authorize(t *testing.T, req *Request, conn connectors.Connector) tokens.Response {
	t.Helper()
	result, err := f.grant.Authorize(t.Context(), req, f.client, conn)
	require.NoError(t, err)
	response, ok := result.(tokens.Response)
	require.True(t, ok)
	return response
}

func (f *karsRefreshFixture) claims(t *testing.T, response tokens.Response) tokens.IDTokenClaims {
	t.Helper()
	require.NotEmpty(t, response.AccessToken)
	require.NotEmpty(t, response.RefreshToken)
	token, err := jose.ParseSigned(response.IDToken, []jose.SignatureAlgorithm{jose.RS256})
	require.NoError(t, err)
	keys, err := f.sig.ValidationKeys(t.Context())
	require.NoError(t, err)
	payload, err := token.Verify(keys[0].Key)
	require.NoError(t, err)
	// Dex serializes a single audience as a string, but tokens.Audience is an
	// issuance-only type without the corresponding JSON decoder.
	var wire struct {
		tokens.IDTokenClaims
		Audience string `json:"aud"`
	}
	require.NoError(t, json.Unmarshal(payload, &wire))
	claims := wire.IDTokenClaims
	claims.Audience = tokens.Audience{wire.Audience}
	require.Equal(t, "https://dex.example.com", claims.Issuer)
	require.Equal(t, tokens.Audience{"client"}, claims.Audience)
	subject, err := tokens.GenSubject("user", "local")
	require.NoError(t, err)
	require.Equal(t, subject, claims.Subject)
	require.Equal(t, f.now().Add(time.Hour).Unix(), claims.Expiry)
	require.NotContains(t, string(payload), "offline-data")
	return claims
}
