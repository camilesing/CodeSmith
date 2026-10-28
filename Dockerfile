# syntax=docker/dockerfile:1
# CodeSmith multi-arch Docker image (#501)
#
# Build:  docker buildx build --platform linux/amd64,linux/arm64 -t codesmith:latest .
# Run:    docker run --rm -it -e CODESMITH_API_KEY -v codesmith-home:/home/codesmith/.codesmith codesmith
#
# API keys MUST be passed at runtime (never baked into the image):
#   docker run --rm -it -e CODESMITH_API_KEY codesmith
# Or mount an env file:
#   docker run --rm -it --env-file .env codesmith

ARG RUST_VERSION=1.88

# ── Stage 1: Build ────────────────────────────────────────────────────
FROM --platform=$BUILDPLATFORM rust:${RUST_VERSION}-slim-bookworm AS builder
ARG TARGETPLATFORM
ARG TARGETARCH
ARG BUILDPLATFORM
ARG CODESMITH_BUILD_SHA

ENV CC_aarch64_unknown_linux_gnu=aarch64-linux-gnu-gcc \
    CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc \
    PKG_CONFIG_ALLOW_CROSS=1 \
    PKG_CONFIG_LIBDIR_aarch64_unknown_linux_gnu=/usr/lib/aarch64-linux-gnu/pkgconfig:/usr/share/pkgconfig \
    CODESMITH_BUILD_SHA=${CODESMITH_BUILD_SHA}

RUN if [ "${TARGETARCH}" = "arm64" ] && [ "${BUILDPLATFORM}" != "${TARGETPLATFORM}" ]; then \
      dpkg --add-architecture arm64; \
    fi \
    && apt-get update \
    && apt-get install -y --no-install-recommends \
      pkg-config libdbus-1-dev \
    && if [ "${TARGETARCH}" = "arm64" ] && [ "${BUILDPLATFORM}" != "${TARGETPLATFORM}" ]; then \
      apt-get install -y --no-install-recommends \
        gcc-aarch64-linux-gnu libc6-dev-arm64-cross libdbus-1-dev:arm64; \
    fi \
    && rm -rf /var/lib/apt/lists/*

# Translate Docker platform into Rust target triple.
# linux/amd64  → x86_64-unknown-linux-gnu
# linux/arm64  → aarch64-unknown-linux-gnu
RUN case "${TARGETPLATFORM}" in \
      linux/amd64)  echo x86_64-unknown-linux-gnu  > /rust-target ;; \
      linux/arm64)  echo aarch64-unknown-linux-gnu > /rust-target ;; \
      *)            echo "Unsupported platform: ${TARGETPLATFORM}" >&2; exit 1 ;; \
    esac

RUN rustup target add "$(cat /rust-target)"

WORKDIR /build
COPY . .

# Build both workspace binaries (the CLI and its app-server companion).
# --locked ensures
# reproducible builds from the committed lockfile.
# Only aarch64 cross-compilation is configured (toolchain + linker env
# above): fail fast on an amd64 cross-build instead of failing later with
# a confusing native-linker error.
RUN if [ "${BUILDPLATFORM}" != "${TARGETPLATFORM}" ] && [ "${TARGETARCH}" = "amd64" ]; then \
      echo "Cross-building linux/amd64 is not supported by this Dockerfile (only arm64 cross-compilation is configured)" >&2; exit 1; \
    fi

RUN --mount=type=cache,id=codesmith-target-${TARGETARCH},target=/build/target,sharing=locked \
    --mount=type=cache,id=codesmith-cargo-registry-${TARGETARCH},target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=codesmith-cargo-git-${TARGETARCH},target=/usr/local/cargo/git,sharing=locked \
    cargo build --release --locked --target "$(cat /rust-target)" \
      -p codesmith-cli -p codesmith-app-server \
    && mkdir -p /out \
    && cp target/$(cat /rust-target)/release/codesmith /out/ \
    && cp target/$(cat /rust-target)/release/codes /out/ \
    && cp target/$(cat /rust-target)/release/codesmith-app-server /out/

# ── Stage 2: Runtime ──────────────────────────────────────────────────
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    libdbus-1-3 \
    && rm -rf /var/lib/apt/lists/*

# Non-root user with explicit UID/GID for filesystem ownership clarity.
RUN groupadd --gid 1000 codesmith \
    && useradd --create-home --shell /bin/bash --uid 1000 --gid 1000 codesmith \
    && install -d -m 0700 -o codesmith -g codesmith /home/codesmith/.codesmith
USER codesmith
WORKDIR /home/codesmith

COPY --from=builder --chown=codesmith:codesmith /out/codesmith /usr/local/bin/codesmith
COPY --from=builder --chown=codesmith:codesmith /out/codes /usr/local/bin/codes
COPY --from=builder --chown=codesmith:codesmith /out/codesmith-app-server /usr/local/bin/codesmith-app-server

# `codes` re-executes the sibling `codesmith`; the dispatcher expects its
# companion binaries next to it. All are in /usr/local/bin — no further path
# setup needed.

ENTRYPOINT ["codesmith"]
CMD []
