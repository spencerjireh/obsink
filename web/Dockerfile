# The website container: the landing page at /, the browser client at /app,
# and a reverse proxy for the API paths so builds that bake the old host keep
# working and the client stays same-origin. Build context is the repository
# root:
#   docker build -f web/Dockerfile .
# Stages: the pure core to wasm, the npm workspaces to a static bundle, Caddy.

FROM rust:1.97-bookworm AS wasm
RUN rustup target add wasm32-unknown-unknown \
    && cargo install wasm-pack --locked --version 0.15.0
WORKDIR /app
COPY . .
RUN wasm-pack build core-wasm --target web

FROM node:22-bookworm-slim AS web
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

FROM caddy:2-alpine
COPY web/Caddyfile /etc/caddy/Caddyfile
COPY site /srv/site
COPY --from=web /app/web/dist /srv/app
EXPOSE 80
