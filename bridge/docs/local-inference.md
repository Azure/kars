# Local inference with AI Runway

Bridge discovers and manages Kars-compatible in-cluster model deployments
through AI Runway. Bridge does not install AI Runway or KAITO.

## Responsibilities

| Component | Responsibility |
|---|---|
| AI Runway / KAITO | ModelDeployment CRDs, engine selection, GPU/CPU workload |
| Kars | Router path, identity, NetworkPolicy, model policy, agent runtime |
| Bridge | Discovery, deployment UX, progress, catalogue, default selection |

## Prerequisites

- AI Runway installed by the cluster operator.
- KAITO or another supported provider/engine.
- GPU nodes and drivers for GPU models.
- Registry and model artifact access.
- A Kars local-inference target matching the model namespace, labels, and ports.

## Workflow

1. Install AI Runway/KAITO using their supported installation process.
2. In Console → Configuration, select **AI Runway (in-cluster)**.
3. Bridge detects the API and lists supported models.
4. Deploy a model from the catalogue.
5. Follow live status until the endpoint is ready.
6. Set the model as default or select it in a launch package.
7. Run a mission and correlate router logs with model-server/GPU telemetry.

## Security

The agent calls only its loopback router. The controller emits a precise
router-to-model NetworkPolicy target. The model service does not require a
credential to be present in the agent container.

## Operations

Plan for:

- model cold-start time;
- GPU capacity and taints/tolerations;
- model cache/storage;
- node and model cost;
- rollout and deletion;
- model provenance and image pinning;
- health and queue saturation.

Bridge’s catalogue UX is not a replacement for operating the underlying model
platform.
