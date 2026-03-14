# ---- Builder stage ----
FROM rust:1.88 AS builder

ARG GIT_HASH=unknown
ARG BUILD_DATE=unknown

WORKDIR /app

# Copy manifests first for dependency caching
COPY Cargo.toml Cargo.lock build.rs ./
COPY crates/ crates/
COPY src/ src/

# Set env vars so build.rs fallback works even without git
ENV GIT_HASH=${GIT_HASH}
ENV BUILD_DATE=${BUILD_DATE}

RUN cargo build --release

# ---- Runtime stage ----
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates \
        chromium \
    && rm -rf /var/lib/apt/lists/*

# chromiumoxide looks for "chromium" or "chromium-browser" on PATH
ENV CHROME_PATH=/usr/bin/chromium

WORKDIR /app

COPY --from=builder /app/target/release/temm1e ./temm1e

EXPOSE 8080

ENTRYPOINT ["./temm1e", "start"]
