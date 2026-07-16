FROM node:24.18.0-alpine AS frontend
WORKDIR /app/front
COPY front/package.json front/package-lock.json ./
RUN npm ci
COPY front/ ./
RUN npm run build

# apk 版 ffmpeg は動画コーデック込みで依存が約 130MB に膨らむため、
# このアプリに必要な機能だけを有効にした最小構成を自前ビルドする。
# 用途は次の 3 つ:
#   - ライブ中継: HLS/HTTPS 入力 → AAC コピー → ADTS 出力 (handlers.rs)
#   - 非 AAC ライブの再エンコード: opus/vorbis デコード → AAC エンコード
#   - yt-dlp の m4a 抽出/fixup: mov/matroska 入力、ipod/mp4 出力、aac_adtstoasc
FROM alpine:latest AS ffmpeg
RUN apk add --no-cache build-base pkgconf nasm curl tar xz openssl-dev zlib-dev
ARG FFMPEG_VERSION=8.1.2
RUN curl -fsSL "https://ffmpeg.org/releases/ffmpeg-${FFMPEG_VERSION}.tar.xz" | tar -xJ
WORKDIR /ffmpeg-${FFMPEG_VERSION}
RUN ./configure \
        --disable-everything \
        --disable-autodetect \
        --disable-doc \
        --disable-debug \
        --disable-ffplay \
        --disable-avdevice \
        --disable-swscale \
        --enable-small \
        --enable-openssl \
        --enable-zlib \
        # udp 自体は不要だが、tls_openssl.c が DTLS 対応で ff_udp_* を
        # 参照するため、リンクを通すのに udp を有効にする必要がある
        --enable-protocol=file,pipe,tcp,udp,tls,http,https,httpproxy,crypto \
        --enable-demuxer=hls,mpegts,mov,matroska,aac \
        --enable-decoder=aac,opus,vorbis \
        --enable-encoder=aac \
        --enable-parser=aac,opus,vorbis \
        --enable-muxer=adts,ipod,mp4 \
        --enable-bsf=aac_adtstoasc \
        --enable-filter=aresample,aformat,anull,abuffer,abuffersink \
    && make -j"$(nproc)" \
    && make install

FROM rust:1-alpine AS backend
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
RUN apk add --no-cache ca-certificates deno python3 libssl3 zlib \
    && apk add --no-cache --virtual .build pipx \
    && pipx install yt-dlp \
    && apk del .build
ENV PATH="/root/.local/bin:${PATH}"
WORKDIR /app
COPY --from=ffmpeg /usr/local/bin/ffmpeg /usr/local/bin/ffprobe /usr/local/bin/
COPY --from=backend /app/target/release/youtube-multiroom .
COPY --from=frontend /app/front/dist front/dist
RUN mkdir audio_cache
EXPOSE 8888
CMD ["./youtube-multiroom"]
