# Build context is the repository root (the server depends on ../core):
#   docker build -f server/Dockerfile .
# Stages: cargo-chef caches the dependency build, then a distroless runtime.

FROM rust:1.97-bookworm AS chef
RUN cargo install cargo-chef --locked
WORKDIR /app

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json --bin obsink-server

FROM chef AS builder
COPY --from=planner /app/recipe.json recipe.json
RUN cargo chef cook --release --recipe-path recipe.json -p obsink-server
COPY . .
RUN cargo build --release -p obsink-server --bin obsink-server \
    && mkdir -p /data \
    && chown 65532:65532 /data

FROM gcr.io/distroless/cc-debian12:nonroot
COPY --from=builder /app/target/release/obsink-server /usr/local/bin/obsink-server
COPY --from=builder --chown=nonroot:nonroot /data /data
ENV OBSINK_DATA_DIR=/data \
    OBSINK_LISTEN=0.0.0.0:8080
VOLUME ["/data"]
EXPOSE 8080
ENTRYPOINT ["obsink-server"]
CMD ["serve"]
