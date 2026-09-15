# Multi-stage build of the sayiir-bench driver.
#
# Build context MUST be the workspace root so we can include the sibling
# sayiir-* path deps. Docker-compose handles this via `context: ..`.

FROM rust:1.98-slim AS build
WORKDIR /build

RUN apt-get update \
 && apt-get install -y --no-install-recommends pkg-config libssl-dev ca-certificates \
 && rm -rf /var/lib/apt/lists/*

# Sibling crates that benchmarks/Cargo.toml resolves via path =.
COPY sayiir-core        /build/sayiir-core
COPY sayiir-persistence /build/sayiir-persistence
COPY sayiir-postgres    /build/sayiir-postgres
COPY sayiir-runtime     /build/sayiir-runtime
COPY sayiir-macros      /build/sayiir-macros

# The bench crate itself.
COPY benchmarks/Cargo.toml /build/benchmarks/Cargo.toml
COPY benchmarks/src        /build/benchmarks/src

WORKDIR /build/benchmarks
RUN cargo build --release --bin sayiir-bench

FROM debian:bookworm-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/*

COPY --from=build /build/benchmarks/target/release/sayiir-bench /usr/local/bin/sayiir-bench
WORKDIR /app
ENTRYPOINT ["/usr/local/bin/sayiir-bench"]
