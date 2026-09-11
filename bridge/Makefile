.PHONY: dev bff web check check-bff check-web check-gateway install images image-gateway images-push helm-lint helm-test helm-install helm-install-kind

# Run BFF + web together (Ctrl-C stops both).
dev:
	@echo "starting kars-bridge-bff (:8081) and web (:3000)"
	@( cd bff && cargo run ) & \
	( cd web && npm run dev ) ; \
	wait

bff:
	cd bff && cargo run

web:
	cd web && npm run dev

install:
	cd web && npm ci
	cd teams-gateway && npm ci

# Component and chart quality gates.
check: check-bff check-web check-gateway helm-lint

check-bff:
	cd bff && cargo build --locked && cargo clippy --locked --all-targets -- -D warnings && cargo test --locked

check-web:
	cd web && npm run lint && npm run build

check-gateway:
	cd teams-gateway && npm run lint && npm run typecheck && npm run build && npm test

# ── Container images + Helm (deploy on any cluster: AKS/EKS/GKE/kind) ─────────
# Local image builds are safe by default; select your registry explicitly to publish.
REGISTRY ?= localhost
TAG ?= latest
KIND_CLUSTER ?= kars-dev

# Build both Bridge images (BFF + web). Override REGISTRY/TAG for your registry.
images:
	docker build -f bff/Dockerfile -t $(REGISTRY)/kars-bridge-bff:$(TAG) bff
	docker build -f web/Dockerfile -t $(REGISTRY)/kars-bridge-web:$(TAG) web

image-gateway:
	docker build -f teams-gateway/Dockerfile -t $(REGISTRY)/kars-bridge-teams-gateway:$(TAG) teams-gateway

images-push: images
	docker push $(REGISTRY)/kars-bridge-bff:$(TAG)
	docker push $(REGISTRY)/kars-bridge-web:$(TAG)

helm-lint:
	helm lint deploy/helm/kars-bridge

# Offline add-on lifecycle regressions, using the existing Vitest runner.
helm-test:
	cd teams-gateway && npm test -- tests/chart.test.ts

# Install (or upgrade) the Bridge additively on an existing kars cluster.
helm-install:
	helm upgrade --install kars-bridge deploy/helm/kars-bridge -n kars-system \
	  --set bff.image.repository=$(REGISTRY)/kars-bridge-bff \
	  --set web.image.repository=$(REGISTRY)/kars-bridge-web \
	  --set bff.image.tag=$(TAG) --set web.image.tag=$(TAG)

# Local kind: build dev images, load them, install with the kind overlay.
helm-install-kind:
	docker build -f bff/Dockerfile -t kars-bridge-bff:dev bff
	docker build -f web/Dockerfile -t kars-bridge-web:dev web
	kind load docker-image kars-bridge-bff:dev kars-bridge-web:dev --name $(KIND_CLUSTER)
	helm upgrade --install kars-bridge deploy/helm/kars-bridge -n kars-system \
	  -f deploy/helm/kars-bridge/values-kind.yaml
