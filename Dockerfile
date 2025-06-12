FROM rust:1.87 AS builder
WORKDIR /usr/src/link_yeeter
COPY . .
RUN cargo build --release

FROM python:3.13
RUN apt-get update && apt-get install -y ffmpeg
RUN pip install yt-dlp
COPY --from=builder /usr/src/link_yeeter/target/release/link_yeeter /usr/local/bin/link_yeeter
CMD ["link_yeeter"]
