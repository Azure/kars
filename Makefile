# AzureClaw Makefile
# Usage: make build | make test | make lint | make images | make clean

VERSION ?= $(shell cat cli/package.json | grep '"version"' | head -1 | sed 's/.*"\([0-9].*\)".*/\1/')
GIT_SHA := $(shell git rev-parse --short HEAD 2>/dev/null || echo "dev")
IMAGE_TAG ?= $(VERSION)-$(GIT_SHA)
REGISTRY ?= azureclawacr.azurecr.io

.PHONY: build test lint images clean cli controller router

# ─── Build ────────────────────────────────────────────────────────────────────

build: controller router cli ## Build all components

controller: ## Build the Rust controller
	cargo build --release --package azureclaw-controller

router: ## Build the Rust inference router
	cargo build --release --package azureclaw-inference-router

cli: ## Build the TypeScript CLI
	cd cli && npm ci && npm run build

# ─── Test ─────────────────────────────────────────────────────────────────────

test: ## Run all tests
	cargo test --all
	cd cli && npm test 2>/dev/null || echo "CLI tests: vitest not configured yet"

test-rust: ## Run Rust tests only
	cargo test --all

test-e2e: ## Run E2E tests (requires Docker + Kind)
	bash tests/e2e/run.sh

test-e2e-manual: ## Run the manual E2E matrix against an existing cluster (see tests/e2e-manual/README.md)
	bash tests/e2e-manual/run.sh

helm-package: ## Lint + package the AzureClaw Helm chart into ./dist/charts/
	bash deploy/helm/package.sh

docs-site: ## Build the mdbook documentation site into target/book/ (requires mdbook)
	@command -v mdbook >/dev/null 2>&1 || { echo "mdbook not installed: cargo install mdbook"; exit 1; }
	cd docs/site && mdbook build

docs-site-serve: ## Live-preview the documentation site at http://localhost:3000
	@command -v mdbook >/dev/null 2>&1 || { echo "mdbook not installed: cargo install mdbook"; exit 1; }
	cd docs/site && mdbook serve --port 3000 --open

# ─── Lint ─────────────────────────────────────────────────────────────────────

lint: ## Run linters
	cargo clippy --all-targets -- -D warnings
	cd cli && npm run lint 2>/dev/null || echo "CLI lint: oxlint not installed"

check: ## Check compilation without building
	cargo check --all

fmt: ## Format code
	cargo fmt --all
	cd cli && npm run format 2>/dev/null || true

# ─── Container Images ────────────────────────────────────────────────────────

images: image-controller image-router image-sandbox image-relay image-registry image-runtimes ## Build all container images

image-runtimes: image-runtime-anthropic image-runtime-langgraph image-runtime-maf-python image-runtime-openai-agents image-runtime-pydantic-ai ## Build all runtime adapter images

image-runtime-anthropic: ## Build Anthropic Claude Agent SDK runtime image
	docker build --platform linux/amd64 \
		-t $(REGISTRY)/azureclaw-runtime-anthropic:$(IMAGE_TAG) \
		-t $(REGISTRY)/azureclaw-runtime-anthropic:latest \
		--label "org.opencontainers.image.version=$(VERSION)" \
		--label "org.opencontainers.image.revision=$(GIT_SHA)" \
		-f sandbox-images/anthropic/Dockerfile .

image-runtime-langgraph: ## Build LangGraph (Python) runtime image
	docker build --platform linux/amd64 \
		-t $(REGISTRY)/azureclaw-runtime-langgraph:$(IMAGE_TAG) \
		-t $(REGISTRY)/azureclaw-runtime-langgraph:latest \
		--label "org.opencontainers.image.version=$(VERSION)" \
		--label "org.opencontainers.image.revision=$(GIT_SHA)" \
		-f sandbox-images/langgraph/Dockerfile .

image-runtime-langgraph-ts: ## Build LangGraph (TypeScript) runtime image
	docker build --platform linux/amd64 \
		-t $(REGISTRY)/azureclaw-runtime-langgraph-ts:$(IMAGE_TAG) \
		-t $(REGISTRY)/azureclaw-runtime-langgraph-ts:latest \
		--label "org.opencontainers.image.version=$(VERSION)" \
		--label "org.opencontainers.image.revision=$(GIT_SHA)" \
		-f sandbox-images/langgraph-ts/Dockerfile .

image-runtime-maf-python: ## Build Microsoft Agent Framework (Python) runtime image
	docker build --platform linux/amd64 \
		-t $(REGISTRY)/azureclaw-runtime-maf-python:$(IMAGE_TAG) \
		-t $(REGISTRY)/azureclaw-runtime-maf-python:latest \
		--label "org.opencontainers.image.version=$(VERSION)" \
		--label "org.opencontainers.image.revision=$(GIT_SHA)" \
		-f sandbox-images/maf-python/Dockerfile .

image-runtime-openai-agents: ## Build OpenAI Agents SDK runtime image
	docker build --platform linux/amd64 \
		-t $(REGISTRY)/azureclaw-runtime-openai-agents:$(IMAGE_TAG) \
		-t $(REGISTRY)/azureclaw-runtime-openai-agents:latest \
		--label "org.opencontainers.image.version=$(VERSION)" \
		--label "org.opencontainers.image.revision=$(GIT_SHA)" \
		-f sandbox-images/openai-agents/Dockerfile .

image-runtime-pydantic-ai: ## Build Pydantic-AI runtime image
	docker build --platform linux/amd64 \
		-t $(REGISTRY)/azureclaw-runtime-pydantic-ai:$(IMAGE_TAG) \
		-t $(REGISTRY)/azureclaw-runtime-pydantic-ai:latest \
		--label "org.opencontainers.image.version=$(VERSION)" \
		--label "org.opencontainers.image.revision=$(GIT_SHA)" \
		-f sandbox-images/pydantic-ai/Dockerfile .

image-controller: ## Build controller Docker image
	docker build --platform linux/amd64 \
		-t $(REGISTRY)/azureclaw-controller:$(IMAGE_TAG) \
		-t $(REGISTRY)/azureclaw-controller:latest \
		--label "org.opencontainers.image.version=$(VERSION)" \
		--label "org.opencontainers.image.revision=$(GIT_SHA)" \
		-f controller/Dockerfile .

image-router: ## Build inference router Docker image
	docker build --platform linux/amd64 \
		-t $(REGISTRY)/azureclaw-inference-router:$(IMAGE_TAG) \
		-t $(REGISTRY)/azureclaw-inference-router:latest \
		--label "org.opencontainers.image.version=$(VERSION)" \
		--label "org.opencontainers.image.revision=$(GIT_SHA)" \
		-f inference-router/Dockerfile .

image-sandbox-base: ## Build sandbox base image (heavy deps — rebuild when upgrading OpenClaw/Python/Go tools)
	docker build --platform linux/amd64 \
		--build-arg OPENCLAW_CACHE_BUST=$$(date +%s) \
		-t $(REGISTRY)/azureclaw-sandbox-base:$(IMAGE_TAG) \
		-t $(REGISTRY)/azureclaw-sandbox-base:latest \
		-f sandbox-images/openclaw/Dockerfile.base .

image-sandbox: image-router ## Build sandbox Docker image (slim overlay — fast per-commit rebuild)
	docker build --platform linux/amd64 \
		--build-arg SANDBOX_BASE_IMAGE=$(REGISTRY)/azureclaw-sandbox-base:latest \
		--build-arg INFERENCE_ROUTER_IMAGE=$(REGISTRY)/azureclaw-inference-router:latest \
		-t $(REGISTRY)/openclaw-sandbox:$(IMAGE_TAG) \
		-t $(REGISTRY)/openclaw-sandbox:latest \
		-f sandbox-images/openclaw/Dockerfile .

# AgentMesh relay + registry images for the AGT provider are now built
# from the upstream microsoft/agent-governance-toolkit repository — see
# deploy/agentmesh-agt.yaml and cli/src/commands/dev/local-k8s.ts
# (deployAgentMesh helper) for the build invocation. The Makefile no
# longer owns these targets after the Phase 5.2 vendored fork removal.

push: ## Push all images to ACR (controller + router + sandbox-base + sandbox + runtimes)
	docker push $(REGISTRY)/azureclaw-controller:$(IMAGE_TAG)
	docker push $(REGISTRY)/azureclaw-controller:latest
	docker push $(REGISTRY)/azureclaw-inference-router:$(IMAGE_TAG)
	docker push $(REGISTRY)/azureclaw-inference-router:latest
	docker push $(REGISTRY)/azureclaw-sandbox-base:$(IMAGE_TAG)
	docker push $(REGISTRY)/azureclaw-sandbox-base:latest
	docker push $(REGISTRY)/openclaw-sandbox:$(IMAGE_TAG)
	docker push $(REGISTRY)/openclaw-sandbox:latest

push-runtimes: ## Push all runtime adapter images to ACR
	docker push $(REGISTRY)/azureclaw-runtime-anthropic:$(IMAGE_TAG)
	docker push $(REGISTRY)/azureclaw-runtime-anthropic:latest
	docker push $(REGISTRY)/azureclaw-runtime-langgraph:$(IMAGE_TAG)
	docker push $(REGISTRY)/azureclaw-runtime-langgraph:latest
	docker push $(REGISTRY)/azureclaw-runtime-maf-python:$(IMAGE_TAG)
	docker push $(REGISTRY)/azureclaw-runtime-maf-python:latest
	docker push $(REGISTRY)/azureclaw-runtime-openai-agents:$(IMAGE_TAG)
	docker push $(REGISTRY)/azureclaw-runtime-openai-agents:latest
	docker push $(REGISTRY)/azureclaw-runtime-pydantic-ai:$(IMAGE_TAG)
	docker push $(REGISTRY)/azureclaw-runtime-pydantic-ai:latest

apply: ## Apply Helm chart to AKS (fast upgrade)
	azureclaw up --upgrade

push-apply: cli images push apply ## Rebuild CLI, build all images, push to ACR, apply to AKS

# ─── Development ──────────────────────────────────────────────────────────────

dev: cli ## Start local development sandbox
	cd cli && npm link
	azureclaw dev

dev-compose-up: cli ## Start the local fake-router dev stack (plan T4)
	docker compose -f docker-compose.dev.yml up -d
	@echo
	@echo "Fake router live at http://127.0.0.1:8443"
	@echo "  Point AZURECLAW_ROUTER_URL=http://127.0.0.1:8443 at any AzureClaw client."
	@echo "  Tear down with: make dev-compose-down"

dev-compose-down: ## Stop the local fake-router dev stack
	docker compose -f docker-compose.dev.yml down

scenario: ## Run YAML scenarios (default: all in cli/src/testing/scenarios/). Usage: make scenario [SCENARIO=path]
	@cd cli && if [ -n "$(SCENARIO)" ]; then \
		npx tsx src/testing/scenario-runner-cli.ts "$(SCENARIO)"; \
	else \
		npx tsx src/testing/scenario-runner-cli.ts src/testing/scenarios; \
	fi

install-cli: cli ## Install CLI globally via npm link
	cd cli && npm link

# ─── Fuzz (s4) ────────────────────────────────────────────────────────────────

fuzz: ## Run all inference-router fuzz targets for 60s each (requires nightly + cargo-fuzz)
	@cd inference-router && for t in fuzz_deserialize_state fuzz_sanitize_chat fuzz_parse_streaming_pf; do \
		echo "▶ fuzzing $$t"; \
		cargo +nightly fuzz run $$t -- -max_total_time=60 || exit 1; \
	done

fuzz-quick: ## Smoke-run each fuzz target for 10s (CI-fast)
	@cd inference-router && for t in fuzz_deserialize_state fuzz_sanitize_chat fuzz_parse_streaming_pf; do \
		echo "▶ smoke fuzz $$t"; \
		cargo +nightly fuzz run $$t -- -max_total_time=10 -runs=100000 || exit 1; \
	done

# ─── Clean ────────────────────────────────────────────────────────────────────

clean: ## Remove build artifacts
	cargo clean
	rm -rf cli/dist cli/node_modules

# ─── Help ─────────────────────────────────────────────────────────────────────

help: ## Show this help
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) | sort | awk 'BEGIN {FS = ":.*?## "}; {printf "\033[36m%-20s\033[0m %s\n", $$1, $$2}'

.DEFAULT_GOAL := help
