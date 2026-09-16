# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

# Context: bridge/idp. Native builds only: build each architecture on its own
# hosted worker. Bookworm's glibc baseline is older than Azure Linux 3's.
FROM golang:1.26.8-bookworm@sha256:9fdc884aacc3bec89b20ffc69f4bb369c78210e3e4f600387b5128b12c199f81 AS source
ENV GOTOOLCHAIN=local GOWORK=off GOPROXY=https://proxy.golang.org \
    GOSUMDB=sum.golang.org GOMODCACHE=/work/mod GOCACHE=/work/cache \
    GOFLAGS=-mod=readonly CGO_ENABLED=1
WORKDIR /src/dex
COPY locks/inputs.lock /packaging/locks/inputs.lock
COPY scripts/fetch-source.sh scripts/source-inventory.sh scripts/apply-source-patches.sh /packaging/scripts/
COPY patches/ /packaging/patches/
RUN sh /packaging/scripts/fetch-source.sh

FROM source AS dependencies
# Deliberately absent until a hosted Go resolver's output has been reviewed.
# A runtime build MUST fail rather than resolve new versions implicitly.
COPY locks/generated/ /locks/
COPY locks/requests.txt /packaging/locks/requests.txt
COPY scripts/verify-locks.sh /packaging/scripts/verify-locks.sh
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
RUN go test -race -count=1 -v \
    -run '^(TestKarsAuthorizationErrorDescriptionsLiteral|TestVerifyUnsignedMessageAndSignedAssertionWithRootXmlNs|TestKarsSAMLFixtureCertificateValidity)$' \
    ./server ./connector/saml

# Kept separate from shipping layers. Full upstream package suites; external
# service integration tests still require the upstream documented services.
FROM compatibility-tests AS upstream-tests
COPY scripts/upstream-tests.sh /packaging/scripts/upstream-tests.sh
RUN sh /packaging/scripts/upstream-tests.sh

FROM build AS test-tools
COPY tests/probe.go tests/probe_test.go /packaging/tests/
# Only the external HTTP probe is pure Go. Dex itself always uses real CGO.
RUN go test -race -count=1 -v /packaging/tests/probe.go /packaging/tests/probe_test.go \
    && CGO_ENABLED=0 go build -trimpath -buildvcs=false -o /out/probe /packaging/tests/probe.go
ENTRYPOINT ["/out/probe"]

FROM mcr.microsoft.com/azurelinux/distroless/base:3.0@sha256:4377af4aa7a810b7d59f691eae5066895a71aa3eee4cfb4eba527bbebff16479 AS runtime
LABEL org.opencontainers.image.title="Kars Dex security rebuild" \
    org.opencontainers.image.description="Dex v2.45.1 with disclosed Kars source, dependency and packaging patches; not an unmodified upstream image" \
    org.opencontainers.image.version="v2.45.1-kars.1" \
    org.opencontainers.image.source="https://github.com/Azure/kars" \
    org.opencontainers.image.licenses="Apache-2.0 AND MIT" \
    io.kars.dex.upstream.revision="11d2eeb52b42e1980e14cb91e69dd9e3faab2076"
COPY --from=build /out/dex /usr/local/bin/dex
COPY --from=build /src/dex/web /srv/dex/web
COPY --from=build /out/doc/ /usr/share/doc/dex/
COPY --from=build --chown=1001:1001 /out/etc/ /etc/dex/
COPY --from=build --chown=1001:1001 /out/data/ /var/dex/
USER 1001:1001
EXPOSE 5556 5557 5558
ENTRYPOINT ["/usr/local/bin/dex"]
CMD ["serve", "/etc/dex/config.yaml"]
