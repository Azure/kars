// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

package lifetime

import (
	"context"
	"fmt"
	"github.com/inspektor-gadget/inspektor-gadget/pkg/operators"
	kubemanager "github.com/inspektor-gadget/inspektor-gadget/pkg/operators/kubemanager"
	"github.com/spf13/viper"
	"reflect"
	"testing"

	containercollection "github.com/inspektor-gadget/inspektor-gadget/pkg/container-collection"
	gadgetcontext "github.com/inspektor-gadget/inspektor-gadget/pkg/gadget-context"
	tracercollection "github.com/inspektor-gadget/inspektor-gadget/pkg/tracer-collection"
)

type lifecycleAttacher struct{ attached, detached int }

func (a *lifecycleAttacher) AttachContainer(*containercollection.Container) error {
	a.attached++
	return nil
}
func (a *lifecycleAttacher) DetachContainer(*containercollection.Container) error {
	a.detached++
	return nil
}

func TestKarsKubeManagerCloseLifecycle(t *testing.T) {
	for _, mode := range []string{"metadata", "started", "startup-aborted"} {
		t.Run(mode, func(t *testing.T) {
			cc := &containercollection.ContainerCollection{}
			if err := cc.Initialize(containercollection.WithPubSub()); err != nil {
				t.Fatal(err)
			}
			defer cc.Close()
			tc, err := tracercollection.NewTracerCollectionTest(cc)
			if err != nil {
				t.Fatal(err)
			}
			defer tc.Close()
			manager := kubemanager.NewKubeManager(cc, tc)
			ctx := gadgetcontext.New(context.Background(), "kars-lifecycle")
			defer ctx.Cancel()
			attacher := &lifecycleAttacher{}
			ctx.SetVar("ebpfInstance", attacher)
			item := &containercollection.Container{}
			item.Runtime.ContainerID = "initial"
			cc.AddContainer(item)
			ctx.SetVar("config", viper.New())
			ctx.SetVar("NeedContainerEvents", true)
			m, err := manager.InstantiateDataOperator(ctx, nil)
			if err != nil || m == nil {
				t.Fatalf("instantiate: instance=%v error=%v", m, err)
			}
			if mode != "metadata" {
				if err := m.(operators.PreStart).PreStart(ctx); err != nil {
					t.Fatal(err)
				}
				if attacher.attached != 1 {
					t.Fatalf("initial attachments=%d", attacher.attached)
				}
				if mode == "started" {
					if err := m.Start(ctx); err != nil {
						t.Fatal(err)
					}
					if err := m.Stop(ctx); err != nil {
						t.Fatal(err)
					}
				}
			}
			if err := m.Close(ctx); err != nil {
				t.Fatal(err)
			}
			subscribers := reflect.ValueOf(cc).Elem().FieldByName("pubsub").Elem().FieldByName("subs").Len()
			if subscribers != 0 {
				t.Errorf("closed gadget retained in container subscribers: %d", subscribers)
			}
			if tc.TracerCount() != 0 {
				t.Errorf("closed gadget retained in tracer collection: %d", tc.TracerCount())
			}
			if attacher.detached != attacher.attached {
				t.Errorf("remaining attachments after Close: attached=%d detached=%d", attacher.attached, attacher.detached)
			}
			before := attacher.attached
			for i := 0; i < 3; i++ {
				extra := &containercollection.Container{}
				extra.Runtime.ContainerID = fmt.Sprintf("after-close-%d", i)
				cc.AddContainer(extra)
			}
			if attacher.attached != before {
				t.Errorf("closed gadget received container events: before=%d after=%d", before, attacher.attached)
			}
		})
	}
}
