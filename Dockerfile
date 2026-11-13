# syntax=docker/dockerfile:1.7

FROM --platform=$BUILDPLATFORM node:26.5.0-bookworm-slim AS web-build
WORKDIR /src/web
COPY web/package.json web/package-lock.json ./
RUN npm install -g npm@11.17.0 && npm ci
COPY web/ ./
RUN npm run build

FROM --platform=$TARGETPLATFORM rust:1.88-bookworm AS server-build
WORKDIR /src
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY crates/ ./crates/
RUN cargo build --release --locked -p app-depot-server

FROM --platform=$TARGETPLATFORM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates libsqlite3-0 \
    && rm -rf /var/lib/apt/lists/*
ARG APP_DEPOT_UID=10001
ARG APP_DEPOT_GID=10001
RUN groupadd --gid "$APP_DEPOT_GID" app-depot \
    && useradd --uid "$APP_DEPOT_UID" --gid "$APP_DEPOT_GID" --no-create-home --shell /usr/sbin/nologin app-depot \
    && mkdir -p /app/web /data \
    && chown "$APP_DEPOT_UID:$APP_DEPOT_GID" /data
COPY --from=server-build /src/target/release/app-depot-server /app/app-depot-server
COPY --from=web-build /src/web/dist/ /app/web/
USER ${APP_DEPOT_UID}:${APP_DEPOT_GID}
ENV APP_DEPOT_DATA_ROOT=/data/production \
    APP_DEPOT_ASSETS=/app/web \
    APP_DEPOT_BIND=0.0.0.0:8787
EXPOSE 8787
ENTRYPOINT ["/app/app-depot-server"]
CMD ["serve", "--mode", "production"]
