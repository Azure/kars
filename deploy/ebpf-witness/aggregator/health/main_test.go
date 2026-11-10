// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

package main

import (
	"context"
	"net"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"google.golang.org/grpc"
	"google.golang.org/grpc/health"
	healthpb "google.golang.org/grpc/health/grpc_health_v1"
)

func server(t *testing.T, service healthpb.HealthServer) string {
	t.Helper()
	socket := filepath.Join(t.TempDir(), "health.sock")
	listener, err := net.Listen("unix", socket)
	if err != nil {
		t.Fatal(err)
	}
	rpc := grpc.NewServer()
	healthpb.RegisterHealthServer(rpc, service)
	go func() {
		if err := rpc.Serve(listener); err != nil {
			t.Errorf("health server: %v", err)
		}
	}()
	t.Cleanup(rpc.Stop)
	return socket
}

func TestHealthStatus(t *testing.T) {
	for _, status := range []healthpb.HealthCheckResponse_ServingStatus{
		healthpb.HealthCheckResponse_SERVING,
		healthpb.HealthCheckResponse_NOT_SERVING,
		healthpb.HealthCheckResponse_UNKNOWN,
		healthpb.HealthCheckResponse_SERVICE_UNKNOWN,
	} {
		t.Run(status.String(), func(t *testing.T) {
			service := health.NewServer()
			service.SetServingStatus("", status)
			socket := server(t, service)
			ctx, cancel := context.WithTimeout(context.Background(), time.Second)
			defer cancel()
			err := check(ctx, socket)
			if status == healthpb.HealthCheckResponse_SERVING {
				if err != nil {
					t.Fatal(err)
				}
			} else if err == nil || !strings.Contains(err.Error(), status.String()) {
				t.Fatalf("expected explicit %s failure, got %v", status, err)
			}
		})
	}
}

func TestMissingSocket(t *testing.T) {
	ctx, cancel := context.WithTimeout(context.Background(), time.Second)
	defer cancel()
	if err := check(ctx, filepath.Join(t.TempDir(), "absent.sock")); err == nil {
		t.Fatal("missing observer accepted")
	}
}

type stalledHealth struct {
	healthpb.UnimplementedHealthServer
}

func (stalledHealth) Check(ctx context.Context, _ *healthpb.HealthCheckRequest) (*healthpb.HealthCheckResponse, error) {
	<-ctx.Done()
	return nil, ctx.Err()
}

func TestDeadlineAndCancellation(t *testing.T) {
	socket := server(t, stalledHealth{})
	ctx, cancel := context.WithTimeout(context.Background(), 50*time.Millisecond)
	defer cancel()
	start := time.Now()
	if err := check(ctx, socket); err == nil || !strings.Contains(err.Error(), "DeadlineExceeded") {
		t.Fatalf("stalled health did not fail with deadline: %v", err)
	}
	if time.Since(start) > time.Second {
		t.Fatal("health RPC exceeded its bounded deadline")
	}
	cancelled, stop := context.WithCancel(context.Background())
	stop()
	if err := check(cancelled, socket); err == nil || !strings.Contains(err.Error(), "Canceled") {
		t.Fatalf("explicit cancellation ignored: %v", err)
	}
}
