# Образ Pult для установки: фронтенд и сервер в одном бинарнике.
# Обычно собирается через compose: docker compose -f deploy/compose.yml up -d --build

FROM node:26-bookworm-slim AS web
WORKDIR /src/web
COPY web/package.json web/package-lock.json ./
RUN npm ci --no-audit --no-fund
COPY web/ ./
RUN npm run build

FROM rust:1.88-bookworm AS server
# libkrb5 и libclang — для входа через домен (GSSAPI) и импорта из SQL Server под Kerberos.
RUN apt-get update -qq && apt-get install -y -qq --no-install-recommends libkrb5-dev libclang-dev
WORKDIR /src
COPY . .
COPY --from=web /src/web/dist web/dist
RUN cargo build --release --locked -p pult-server

FROM debian:bookworm-slim
RUN apt-get update -qq \
    && apt-get install -y -qq --no-install-recommends ca-certificates libgssapi-krb5-2 postgresql-common \
    && /usr/share/postgresql-common/pgdg/apt.postgresql.org.sh -y \
    && apt-get install -y -qq --no-install-recommends postgresql-client-17 \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --home-dir /data --no-create-home pult \
    && mkdir /data && chown pult:pult /data
COPY --from=server /src/target/release/pult /usr/local/bin/pult
# Ключ шифрования и сертификат создаются в /data при первом запуске. Этот том нужно хранить и бэкапить.
ENV PULT_DATA_DIR=/data PULT_AUTO_TLS=1 PULT_LISTEN=0.0.0.0:8443 PULT_LOG_FORMAT=text NO_COLOR=1
VOLUME /data
USER pult
WORKDIR /data
EXPOSE 8443
# Миграции встроены в бинарник и идут под advisory lock — безопасно при каждом старте.
CMD ["sh", "-c", "pult migrate && exec pult serve"]
