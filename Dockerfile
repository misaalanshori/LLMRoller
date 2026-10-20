# syntax=docker/dockerfile:1.7

ARG RUST_IMAGE=rust:1.99.0-bookworm@sha256:114c7a4425406451c2866b6aafe69fe29b1b298832db1277d411ac73c82d04d6
ARG DISTROLESS_IMAGE=gcr.io/distroless/static-debian12:nonroot@sha256:e8a4044e0b4ae4257efa45fc026c0bc30ad320d43bd4c1a7d5271bd241e386d0
ARG DEBIAN_SLIM_IMAGE=debian:bookworm-slim@sha256:f06537653ac770703bc45b4b113475bd402f451e85223f0f2837acbf89ab020a

# -----------------------------------------------------------------------------
# Stage 1: planner - compute deterministic cargo-chef dependency recipe
# -----------------------------------------------------------------------------
FROM ${RUST_IMAGE} AS planner
WORKDIR /src
RUN cargo install cargo-chef --locked --version 0.1.78
COPY Cargo.toml Cargo.lock deny.toml ./
COPY xtask/Cargo.toml xtask/Cargo.toml
COPY crates/core/Cargo.toml crates/core/Cargo.toml
COPY crates/proto/Cargo.toml crates/proto/Cargo.toml
COPY crates/store/Cargo.toml crates/store/Cargo.toml
COPY crates/proxy/Cargo.toml crates/proxy/Cargo.toml
COPY crates/admin/Cargo.toml crates/admin/Cargo.toml
COPY crates/server/Cargo.toml crates/server/Cargo.toml
RUN mkdir -p xtask/src crates/core/src crates/proto/src crates/store/src              crates/proxy/src crates/admin/src crates/server/src     && touch xtask/src/main.rs crates/core/src/lib.rs crates/proto/src/lib.rs              crates/store/src/lib.rs crates/proxy/src/lib.rs              crates/admin/src/lib.rs crates/server/src/main.rs     && cargo chef prepare --recipe-path recipe.json

# -----------------------------------------------------------------------------
# Stage 2: builder - compile static musl release binary with embedded UI assets
# -----------------------------------------------------------------------------
FROM ${RUST_IMAGE} AS builder
ARG TARGETARCH
ARG GIT_SHA=unknown
WORKDIR /src

RUN apt-get update && apt-get install -y --no-install-recommends         musl-tools pkg-config clang llvm lld     && rm -rf /var/lib/apt/lists/*     && cargo install cargo-chef --locked --version 0.1.78

RUN case "${TARGETARCH:-amd64}" in       amd64) echo "x86_64-unknown-linux-musl" > /tmp/rust_target ;;       arm64) echo "aarch64-unknown-linux-musl" > /tmp/rust_target ;;       *) echo "Unsupported TARGETARCH: ${TARGETARCH}" >&2; exit 1 ;;     esac     && rustup target add "$(cat /tmp/rust_target)"

COPY --from=planner /src/recipe.json recipe.json
RUN TARGET_TRIPLE="$(cat /tmp/rust_target)"     && cargo chef cook --release --locked --target "${TARGET_TRIPLE}" --recipe-path recipe.json

COPY Cargo.toml Cargo.lock deny.toml ./
COPY presets ./presets
COPY docs ./docs
COPY fixtures ./fixtures
COPY xtask ./xtask
COPY crates ./crates

ENV LLMROLLER_BUILD_GIT_SHA=${GIT_SHA}
RUN TARGET_TRIPLE="$(cat /tmp/rust_target)"     && cargo build --release --locked --target "${TARGET_TRIPLE}" --bin llmroller     && cp "target/${TARGET_TRIPLE}/release/llmroller" /usr/local/bin/llmroller     && strip --strip-debug /usr/local/bin/llmroller     && install -d -m 0750 -o 65532 -g 65532 /staging/var/lib/llmroller     && install -d -m 0755 -o 65532 -g 65532 /staging/etc/llmroller/presets     && install -m 0644 -o 65532 -g 65532 presets/*.toml /staging/etc/llmroller/presets/

# -----------------------------------------------------------------------------
# Stage 3: runtime-dev - instrumented image for local Compose & CI smoke tests
# -----------------------------------------------------------------------------
FROM ${DEBIAN_SLIM_IMAGE} AS runtime-dev
RUN apt-get update && apt-get install -y --no-install-recommends         ca-certificates curl sqlite3 tini     && rm -rf /var/lib/apt/lists/*     && groupadd -g 65532 llmroller     && useradd -u 65532 -g 65532 -d /var/lib/llmroller -s /usr/sbin/nologin -M llmroller

COPY --from=builder --chown=65532:65532 /staging/var/lib/llmroller /var/lib/llmroller
COPY --from=builder --chown=65532:65532 /staging/etc/llmroller /etc/llmroller
COPY --from=builder /usr/local/bin/llmroller /usr/local/bin/llmroller

ENV LLMROLLER_DATA_DIR=/var/lib/llmroller     LLMROLLER_LISTEN=0.0.0.0:8080     LLMROLLER_ADMIN_LISTEN=0.0.0.0:8081     LLMROLLER_METRICS_LISTEN=0.0.0.0:9090     LLMROLLER_DEFAULT_ROUTING_STRATEGY=smooth_weighted_rr     LLMROLLER_LOG=info,llmroller=info

EXPOSE 8080 8081 9090
USER 65532:65532
WORKDIR /var/lib/llmroller

HEALTHCHECK --interval=10s --timeout=3s --start-period=10s --retries=3   CMD ["/usr/local/bin/llmroller", "healthcheck", "--url", "http://127.0.0.1:8080/readyz"]

ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/llmroller"]
CMD ["serve"]

# -----------------------------------------------------------------------------
# Stage 4: runtime-distroless - default hardened production image (< 50 MB)
# -----------------------------------------------------------------------------
FROM ${DISTROLESS_IMAGE} AS runtime-distroless

COPY --from=builder --chown=65532:65532 /staging/var/lib/llmroller /var/lib/llmroller
COPY --from=builder --chown=65532:65532 /staging/etc/llmroller /etc/llmroller
COPY --from=builder /usr/local/bin/llmroller /usr/local/bin/llmroller

ENV LLMROLLER_DATA_DIR=/var/lib/llmroller     LLMROLLER_LISTEN=0.0.0.0:8080     LLMROLLER_ADMIN_LISTEN=0.0.0.0:8081     LLMROLLER_METRICS_LISTEN=0.0.0.0:9090     LLMROLLER_DEFAULT_ROUTING_STRATEGY=smooth_weighted_rr     LLMROLLER_LOG=info,llmroller=info

EXPOSE 8080 8081 9090
USER 65532:65532
WORKDIR /var/lib/llmroller

HEALTHCHECK --interval=10s --timeout=3s --start-period=10s --retries=3   CMD ["/usr/local/bin/llmroller", "healthcheck", "--url", "http://127.0.0.1:8080/readyz"]

ENTRYPOINT ["/usr/local/bin/llmroller"]
CMD ["serve"]
