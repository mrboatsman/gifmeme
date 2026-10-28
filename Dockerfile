# syntax=docker/dockerfile:1
# gifmeme: Rust backend + SvelteKit SPA + ffmpeg in one image.
#
#   docker build -t gifmeme:dev .
#
# CI (.github/workflows/ci.yml) publishes ghcr.io/<owner>/gifmeme.

# --- SPA (arch independent, runs on the build host) -------------------------
FROM --platform=$BUILDPLATFORM node:24-slim AS web
ENV PNPM_HOME=/pnpm PATH=/pnpm:$PATH CI=true
RUN corepack enable
WORKDIR /web
COPY web/package.json web/pnpm-lock.yaml web/pnpm-workspace.yaml ./
RUN pnpm install --frozen-lockfile
COPY web/ ./
RUN pnpm run build

# --- backend -----------------------------------------------------------------
FROM rust:1-trixie AS rust
WORKDIR /src
# Dependency layer: build against a stub main so source edits don't
# recompile every crate.
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo 'fn main() {}' > src/main.rs \
    && cargo build --release --locked \
    && rm -rf src target/release/deps/gifmeme* target/release/gifmeme*
COPY migrations ./migrations
COPY src ./src
RUN cargo build --release --locked && strip target/release/gifmeme

# --- runtime -----------------------------------------------------------------
FROM debian:trixie-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ffmpeg ca-certificates tini \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --home-dir /data gifmeme \
    && mkdir -p /data && chown gifmeme /data
COPY --from=rust /src/target/release/gifmeme /usr/local/bin/gifmeme
COPY --from=web /web/build /app/web
ARG GIT_HASH=dev
ENV GIFMEME_LISTEN=0.0.0.0:8080 \
    GIFMEME_DATA_DIR=/data \
    GIFMEME_STATIC_DIR=/app/web \
    GIFMEME_VERSION=${GIT_HASH}
USER 10001
VOLUME /data
EXPOSE 8080
ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/gifmeme"]
