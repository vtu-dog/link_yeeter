ARG APP_VERSION=3.3.0

FROM rust:1.94 AS chef
RUN cargo install cargo-chef
WORKDIR /usr/src/link_yeeter

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder
COPY --from=planner /usr/src/link_yeeter/recipe.json recipe.json
RUN cargo chef cook --release --recipe-path recipe.json
COPY . .
RUN cargo build --release

FROM python:3.14-slim
ARG APP_VERSION
LABEL version="$APP_VERSION"
RUN apt-get update && apt-get install -y ffmpeg \
    && apt-get clean && rm -rf /var/lib/apt/lists/*
RUN pip install --no-cache-dir yt-dlp
COPY --from=builder /usr/src/link_yeeter/target/release/link_yeeter /usr/local/bin/link_yeeter
CMD ["link_yeeter"]
