# The website container: the landing page at /, the browser client at /app,
# and a reverse proxy for the API paths so builds that bake the old host keep
# working and the client stays same-origin. Build context is the repository
# root:
#   docker build -f web/Dockerfile .
# Stages: the pure core to wasm, the npm workspaces to a static bundle, Caddy.
# Base images are pinned by digest; bump the tag and digest together.

FROM rust:1.97-bookworm@sha256:0e2bcaef56d041a486784e54104a81aebe0da44bd03019bd70bc0401e42e4a97 AS wasm
ARG TARGETARCH
RUN rustup target add wasm32-unknown-unknown
# wasm-pack from the prebuilt release (compiling it from source takes minutes).
RUN set -eu; \
    case "$TARGETARCH" in \
        amd64) arch=x86_64; sha=c09f971ecaed9a2efc80fdcea7a00ef6b53c7fadc8c57d1f61b53a6aa66b668a ;; \
        arm64) arch=aarch64; sha=e17ef0806381c3a0acb9c9ddad643a49facaa5a2ecf657a421d4d8f3357a24b7 ;; \
        *) echo "unsupported TARGETARCH: $TARGETARCH" >&2; exit 1 ;; \
    esac; \
    name="wasm-pack-v0.15.0-$arch-unknown-linux-musl"; \
    curl -fsSL -o /tmp/wasm-pack.tar.gz "https://github.com/rustwasm/wasm-pack/releases/download/v0.15.0/$name.tar.gz"; \
    echo "$sha  /tmp/wasm-pack.tar.gz" | sha256sum -c -; \
    tar -xzf /tmp/wasm-pack.tar.gz -C /tmp; \
    install -m 755 "/tmp/$name/wasm-pack" /usr/local/bin/wasm-pack; \
    rm -rf /tmp/wasm-pack.tar.gz "/tmp/$name"
WORKDIR /app
# Only the Rust inputs: edits to site/, web/ or ui/ keep this layer cached.
# The other workspace members are copied for their manifests (cargo loads the
# whole workspace) but are not built. rust-toolchain.toml is .dockerignore'd;
# the image's own toolchain is used.
COPY Cargo.toml Cargo.lock ./
COPY core ./core
COPY core-wasm ./core-wasm
COPY cli ./cli
COPY server ./server
COPY mobile ./mobile
COPY desktop/src-tauri ./desktop/src-tauri
RUN wasm-pack build core-wasm --target web

FROM node:22-bookworm-slim@sha256:48e4b67d85f87bd551df43704e24d252f56cc5f8e9718841aace50f19948f0f9 AS web
WORKDIR /app
COPY package.json package-lock.json ./
COPY ui/package.json ui/
COPY desktop/package.json desktop/
COPY web/package.json web/
# Browsers are only for the e2e harness; the Tauri CLI download is skipped too.
ENV PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD=1
RUN npm ci --ignore-scripts && npm rebuild esbuild
COPY ui ./ui
COPY web ./web
COPY --from=wasm /app/core-wasm/pkg ./core-wasm/pkg
RUN npm run build -w web

FROM caddy:2-alpine@sha256:de23def33b17fb5d1290b0f6c2add1d70780e52341896c00a4c8a2a2fe9d355e
COPY web/Caddyfile /etc/caddy/Caddyfile
COPY site /srv/site
COPY --from=web /app/web/dist /srv/app
EXPOSE 80
# The landing page needs no backend, so this proves Caddy and the static files.
HEALTHCHECK --interval=30s --timeout=5s --start-period=5s \
    CMD wget -qO- --header='Accept: text/html' http://localhost/ >/dev/null || exit 1
