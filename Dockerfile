FROM node:24-alpine AS frontend
WORKDIR /app/front
COPY front/package.json front/package-lock.json ./
RUN npm ci
COPY front/ ./
RUN npm run build

FROM rust:1.96-alpine AS backend
# openssl クレート (Alexa 署名検証) のビルドに必要
RUN apk add --no-cache musl-dev pkgconf openssl-dev
# 動的リンクにして実行ステージの libssl を共有する (crt-static だと libssl とリンクできない)
ENV RUSTFLAGS="-C target-feature=-crt-static"
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo 'fn main() {}' > src/main.rs && cargo build --release && rm -rf src
COPY src/ src/
RUN touch src/main.rs && cargo build --release

FROM alpine:latest
# deno は yt-dlp の JS ランタイムとして必要。apk 版 yt-dlp は更新が遅れる
# 可能性があるため、従来どおり pipx で PyPI の最新版を入れる
RUN apk add --no-cache ca-certificates ffmpeg deno python3 libssl3 \
    && apk add --no-cache --virtual .build pipx \
    && pipx install yt-dlp \
    && apk del .build
ENV PATH="/root/.local/bin:${PATH}"
WORKDIR /app
COPY --from=backend /app/target/release/youtube-multiroom .
COPY --from=frontend /app/front/dist front/dist
RUN mkdir audio_cache
EXPOSE 8888
CMD ["./youtube-multiroom"]
