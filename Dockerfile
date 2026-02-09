FROM rust:1.93 AS builder
WORKDIR /usr/src/link_yeeter
COPY . .
RUN cargo build --release

FROM python:3.14-slim
RUN apt-get update && apt-get install -y ffmpeg \
    && apt-get clean && rm -rf /var/lib/apt/lists/*
RUN pip install --no-cache-dir yt-dlp
COPY --from=builder /usr/src/link_yeeter/target/release/link_yeeter /usr/local/bin/link_yeeter
CMD ["link_yeeter"]
