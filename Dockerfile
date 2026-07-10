# CamelMailer — single image for every process role.
#
# The same image runs each component depending on the command:
#   docker run camelmailer web-server | smtp-server | worker | initialize
#
# Build:  docker build -t camelmailer .
# The whole workspace is pure Rust (rustls everywhere) — no OpenSSL or other
# native libraries are required at build or run time.

# --------------------------------------------------------------- builder
FROM rust:1.94-slim AS builder

WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates

# BuildKit cache mounts keep the registry + target directory between builds
# so incremental image rebuilds only recompile what changed.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/build/target \
    cargo build --release -p camelmailer \
    && cp target/release/camelmailer /usr/local/bin/camelmailer

# --------------------------------------------------------------- runtime
FROM debian:bookworm-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --create-home --home-dir /camelmailer camelmailer

COPY --from=builder /usr/local/bin/camelmailer /usr/local/bin/camelmailer

USER camelmailer
WORKDIR /camelmailer

# Bind the web server on all interfaces inside the container; override per
# environment. The SMTP server binds `::` by default already.
ENV BIND_ADDRESS=0.0.0.0

# 5000 = HTTP (Admin API + Server API + tracking), 25 = SMTP
EXPOSE 5000 25

ENTRYPOINT ["camelmailer"]
CMD ["web-server"]
