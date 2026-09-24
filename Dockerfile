FROM rust:1-slim-trixie AS builder
RUN apt-get update && apt-get install -y --no-install-recommends pkg-config && rm -rf /var/lib/apt/lists/*
WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY deploy/check-codex.py ./deploy/check-codex.py
COPY ui ./ui
RUN cargo build --locked --release --jobs 2

FROM debian:trixie-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl python3 openssh-client qemu-system-x86 qemu-utils genisoimage xz-utils util-linux && rm -rf /var/lib/apt/lists/* && useradd --uid 1000 --create-home kindred
COPY --from=builder /build/target/release/kindred /usr/local/bin/kindred
COPY harness /opt/kindred/source/harness
COPY deploy /opt/kindred/source/deploy
RUN sh /opt/kindred/source/deploy/install-pi.sh && mkdir -p /opt/kindred/guest /usr/local/lib/kindred && cp -a /opt/kindred/source/deploy/. /opt/kindred/guest/ && cp /usr/local/bin/kindred /opt/kindred/guest/kindred && python3 /opt/kindred/source/deploy/download-codex.py /opt/kindred/guest/codex && install -m 755 /opt/kindred/source/deploy/vm-manager.py /usr/local/lib/kindred/vm-manager.py
ENV KINDRED_PROFILES_DIR=/data/profiles KINDRED_GUEST_SOFTWARE=/opt/kindred/guest
EXPOSE 9444
VOLUME /data
ENTRYPOINT ["python3","/opt/kindred/source/deploy/container-entry.py"]
