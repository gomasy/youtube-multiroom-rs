FROM node:24.18.0-alpine AS frontend
WORKDIR /app/front
COPY front/package.json front/package-lock.json ./
RUN npm ci
COPY front/ ./
RUN npm run build

# The apk-provided ffmpeg balloons to ~130MB once video codecs are included,
# so we build a minimal configuration with only the features this app needs.
# It is used for three purposes:
#   - Live relay: HLS/HTTPS input -> AAC copy -> ADTS output (handlers.rs)
#   - Re-encoding non-AAC live streams: opus/vorbis decode -> AAC encode
#   - yt-dlp m4a extraction/fixup: mov/matroska input, ipod/mp4 output, aac_adtstoasc
FROM alpine:latest AS ffmpeg
RUN apk add --no-cache build-base pkgconf nasm curl tar xz openssl-dev zlib-dev
# renovate: datasource=github-tags depName=FFmpeg/FFmpeg extractVersion=^n(?<version>\d+\.\d+(\.\d+)?)$
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
        # udp is not needed on its own, but tls_openssl.c references ff_udp_*
        # for DTLS support, so it must be enabled to link successfully
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
# Required to build the openssl crate (Alexa request signature verification)
RUN apk add --no-cache musl-dev pkgconf openssl-dev
# Link dynamically so the runtime stage can share its libssl (crt-static cannot link against libssl)
ENV RUSTFLAGS="-C target-feature=-crt-static"
WORKDIR /app
COPY Cargo.toml Cargo.lock build.rs ./
RUN mkdir src && echo 'fn main() {}' > src/main.rs && cargo build --release && rm -rf src
COPY src/ src/
# locales/ is embedded into the binary at compile time, so include it in the build context
COPY locales/ locales/
# Pass git metadata gathered on the host (.git is excluded from the image, so
# build.rs cannot obtain it itself). Falls back to "unknown" if not provided.
ARG GIT_HASH
ARG BUILD_DATE
ENV GIT_HASH=${GIT_HASH} BUILD_DATE=${BUILD_DATE}
RUN touch src/main.rs && cargo build --release

FROM alpine:latest
# deno is needed as yt-dlp's JS runtime. The apk yt-dlp may lag behind, so we
# install the latest PyPI release via pipx as before.
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
