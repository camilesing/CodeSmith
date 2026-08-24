# syntax=docker/dockerfile:1
# CodeSmith multi-arch Docker image (#501)
#
# Build:  docker buildx build --platform linux/amd64,linux/arm64 -t codesmith:latest .
# Run:    docker run --rm -it -e DEEPSEEK_API_KEY -v codesmith-home:/home/codesmith/.codesmith codesmith
#
# API keys MUST be passed at runtime (never baked into the image):
#   docker run --rm -it -e DEEPSEEK_API_KEY codesmith
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

# Build both binaries for the target platform.  --locked ensures
# reproducible builds from the committed lockfile.
RUN --mount=type=cache,id=codesmith-target-${TARGETARCH},target=/build/target,sharing=locked \
    --mount=type=cache,id=codesmith-cargo-registry-${TARGETARCH},target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=codesmith-cargo-git-${TARGETARCH},target=/usr/local/cargo/git,sharing=locked \
    cargo build --release --locked --target "$(cat /rust-target)" \
      -p codesmith-cli -p codesmith-tui \
    && mkdir -p /out \
    && cp target/$(cat /rust-target)/release/codesmith /out/ \
    && cp target/$(cat /rust-target)/release/codesmith-tui /out/

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
COPY --from=builder --chown=codesmith:codesmith /out/codesmith-tui /usr/local/bin/codesmith-tui

# The dispatcher expects to find its companion binary next to it.
# Both are in /usr/local/bin — no further path setup needed.

ENTRYPOINT ["codesmith"]
CMD []
