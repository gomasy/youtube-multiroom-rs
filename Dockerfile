FROM node:24-slim AS frontend
WORKDIR /app/front
COPY front/package.json front/package-lock.json ./
RUN npm ci
COPY front/ ./
RUN npm run build

FROM rust:1.87-slim AS backend
# openssl クレート (Alexa 署名検証) のビルドに必要
RUN apt-get update && apt-get install -y --no-install-recommends \
        pkg-config libssl-dev \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo 'fn main() {}' > src/main.rs && cargo build --release && rm -rf src
COPY src/ src/
RUN touch src/main.rs && cargo build --release

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates curl ffmpeg libssl3 python3 pipx unzip \
    && curl -fsSL https://deno.land/install.sh | DENO_INSTALL=/usr/local sh \
    && pipx install yt-dlp \
    && apt-get purge -y curl pipx unzip \
    && apt-get autoremove -y \
    && rm -rf /var/lib/apt/lists/*
ENV PATH="/root/.local/bin:${PATH}"
WORKDIR /app
COPY --from=backend /app/target/release/youtube-multiroom .
COPY --from=frontend /app/front/dist front/dist
RUN mkdir audio_cache
EXPOSE 8888
CMD ["./youtube-multiroom"]
