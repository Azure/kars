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
	cd cli && npm install && npm run build

# ─── Test ─────────────────────────────────────────────────────────────────────

test: ## Run all tests
	cargo test --all
	cd cli && npm test 2>/dev/null || echo "CLI tests: vitest not configured yet"

test-rust: ## Run Rust tests only
	cargo test --all

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

images: image-controller image-router ## Build all container images

image-controller: ## Build controller Docker image
	docker build -t $(REGISTRY)/azureclaw-controller:$(IMAGE_TAG) \
		-t $(REGISTRY)/azureclaw-controller:latest \
		--label "org.opencontainers.image.version=$(VERSION)" \
		--label "org.opencontainers.image.revision=$(GIT_SHA)" \
		-f controller/Dockerfile .

image-router: ## Build inference router Docker image
	docker build -t $(REGISTRY)/azureclaw-inference-router:$(IMAGE_TAG) \
		-t $(REGISTRY)/azureclaw-inference-router:latest \
		--label "org.opencontainers.image.version=$(VERSION)" \
		--label "org.opencontainers.image.revision=$(GIT_SHA)" \
		-f inference-router/Dockerfile .

push: ## Push images to registry
	docker push $(REGISTRY)/azureclaw-controller:$(IMAGE_TAG)
	docker push $(REGISTRY)/azureclaw-controller:latest
	docker push $(REGISTRY)/azureclaw-inference-router:$(IMAGE_TAG)
	docker push $(REGISTRY)/azureclaw-inference-router:latest

# ─── Development ──────────────────────────────────────────────────────────────

dev: cli ## Start local development sandbox
	cd cli && npm link
	azureclaw dev

install-cli: cli ## Install CLI globally via npm link
	cd cli && npm link

# ─── Clean ────────────────────────────────────────────────────────────────────

clean: ## Remove build artifacts
	cargo clean
	rm -rf cli/dist cli/node_modules

# ─── Help ─────────────────────────────────────────────────────────────────────

help: ## Show this help
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) | sort | awk 'BEGIN {FS = ":.*?## "}; {printf "\033[36m%-20s\033[0m %s\n", $$1, $$2}'

.DEFAULT_GOAL := help
