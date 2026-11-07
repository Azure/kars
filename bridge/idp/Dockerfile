# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

# Context: bridge/idp. Native builds only: build each architecture on its own
# hosted worker. Bookworm's glibc baseline is older than Azure Linux 3's.
FROM golang:1.27.3-bookworm@sha256:d9b6a1beb9474adc2a1ce842573639b44b2a0dd49b6669f51d3821baf9ec37c9 AS source
ENV GOTOOLCHAIN=local GOWORK=off GOPROXY=https://proxy.golang.org \
    GOSUMDB=sum.golang.org GOMODCACHE=/work/mod GOCACHE=/work/cache \
    GOFLAGS=-mod=readonly CGO_ENABLED=1
RUN apt-get update && apt-get install -y --no-install-recommends patch \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /src/dex
COPY locks/inputs.lock /packaging/locks/inputs.lock
COPY scripts/fetch-source.sh scripts/source-inventory.sh scripts/apply-source-patches.sh scripts/verify-source-toolchain.sh /packaging/scripts/
COPY patches/ /packaging/patches/
RUN sh /packaging/scripts/fetch-source.sh

FROM source AS dependencies
# A runtime build MUST use the reviewed locks, never resolve new versions.
COPY locks/generated/ /locks/
COPY locks/requests.txt /packaging/locks/requests.txt
COPY scripts/verify-locks.sh scripts/verify-toolchain.sh /packaging/scripts/
RUN sh /packaging/scripts/verify-locks.sh \
    && go mod download && go mod verify \
    && cmp go.mod /locks/go.mod && cmp go.sum /locks/go.sum \
    && cd api/v2 && go mod download && go mod verify \
    && cmp go.mod /locks/api/v2/go.mod && cmp go.sum /locks/api/v2/go.sum

FROM dependencies AS build
COPY scripts/build.sh /packaging/scripts/build.sh
COPY scripts/notices.go scripts/notices_test.go /packaging/scripts/
COPY NOTICE LICENSE README.md /packaging/
RUN go test -count=1 /packaging/scripts/notices.go /packaging/scripts/notices_test.go \
    && sh /packaging/scripts/build.sh

FROM build AS compatibility-tests
RUN go test -race -count=1 -timeout=120s -v \
    -run '^(TestKarsAuthorizationErrorDescriptionsLiteral|TestVerifyUnsignedMessageAndSignedAssertionWithRootXmlNs|TestKarsSAMLFixtureCertificateValidity|TestKarsRefreshStorageDeadlock)$' \
    ./server/authflow ./connector/saml ./server/grants

# Exercise the released session policy, cookies, expiry and SSO/MFA decisions.
FROM compatibility-tests AS session-tests
RUN go test -race -count=1 -v ./server/session ./server/authflow

# Kept separate from shipping layers. Full upstream package suites; external
# service integration tests still require the upstream documented services.
FROM session-tests AS upstream-tests
COPY scripts/upstream-tests.sh /packaging/scripts/upstream-tests.sh
RUN sh /packaging/scripts/upstream-tests.sh

FROM build AS test-tools
COPY tests/probe.go tests/probe_test.go /packaging/tests/
# Only the external HTTP probe is pure Go. Dex itself always uses real CGO.
RUN go test -race -count=1 -v /packaging/tests/probe.go /packaging/tests/probe_test.go \
    && CGO_ENABLED=0 go build -trimpath -buildvcs=false -o /out/probe /packaging/tests/probe.go
ENTRYPOINT ["/out/probe"]

FROM mcr.microsoft.com/azurelinux/distroless/base:3.0@sha256:6b85a86416a29cdcfd404e9fbd9c5ef49cc0612882cecc702535dcac39c9446c AS runtime
LABEL org.opencontainers.image.title="Kars Dex security rebuild" \
    org.opencontainers.image.description="Dex v2.46.1 with disclosed Kars test, dependency and packaging patches; not an unmodified upstream image" \
    org.opencontainers.image.version="v2.46.1-kars.1" \
    org.opencontainers.image.source="https://github.com/Azure/kars" \
    org.opencontainers.image.licenses="Apache-2.0 AND MIT" \
    io.kars.dex.upstream.revision="75eb19238e10fc4750e7efb05a4031b254b04029"
COPY --from=build /out/dex /usr/local/bin/dex
COPY --from=build /src/dex/web /srv/dex/web
COPY --from=build /out/doc/ /usr/share/doc/dex/
COPY --from=build --chown=1001:1001 /out/etc/ /etc/dex/
COPY --from=build --chown=1001:1001 /out/data/ /var/dex/
USER 1001:1001
EXPOSE 5556 5557 5558
ENTRYPOINT ["/usr/local/bin/dex"]
CMD ["serve", "/etc/dex/config.yaml"]
