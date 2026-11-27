# Container image of the matter-rs service.
#
# Build from the repository root:
#
#   docker build -f packaging/container/Dockerfile -t matter-rs .
#
# Run with the host network: Matter commissioning finds the node over multicast DNS, which does
# not cross Docker's bridge network.
#
#   docker run -d --name matter-rs --network host \
#     -v /srv/matter-rs/config.toml:/etc/matter-rs/config.toml:ro \
#     -v matter-rs-state:/var/lib/matter-rs \
#     matter-rs
#
# A starter configuration comes from the image itself; the service user (uid 10001) must be
# able to read it, and nobody else should, because it holds the setup passcode:
#
#   docker run --rm --user "$(id -u)" -v "$PWD:/out" matter-rs init --output /out/config.toml
#   sudo chown 10001:10001 config.toml && sudo chmod 0600 config.toml

# --- build stage: the toolchain pinned in rust-toolchain.toml ----------------------------------
FROM debian:bookworm-slim AS build

RUN apt-get update \
    && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
        ca-certificates curl gcc libc6-dev \
    && rm -rf /var/lib/apt/lists/*

ENV RUSTUP_HOME=/usr/local/rustup \
    CARGO_HOME=/usr/local/cargo \
    PATH=/usr/local/cargo/bin:$PATH \
    CARGO_INCREMENTAL=0

RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
    | sh -s -- -y --no-modify-path --profile minimal --default-toolchain none

WORKDIR /src
# The toolchain file first, so that the toolchain layer is reused while the sources change.
COPY rust-toolchain.toml ./
RUN rustup toolchain install && rustc --version

COPY . .
RUN cargo build --release --locked -p matter-rs \
    && install -D -m 0755 target/release/matter-rs /out/matter-rs

# --- final stage -------------------------------------------------------------------------------
FROM debian:bookworm-slim

RUN groupadd --system --gid 10001 matter-rs \
    && useradd --system --uid 10001 --gid matter-rs --home-dir /var/lib/matter-rs \
        --shell /usr/sbin/nologin matter-rs \
    && install -d -m 0755 /etc/matter-rs \
    && install -d -m 0700 -o matter-rs -g matter-rs /var/lib/matter-rs

COPY --from=build /out/matter-rs /usr/bin/matter-rs

# The configuration is read from /etc/matter-rs/config.toml (the Linux default); the state
# directory of the non-root service user resolves to /var/lib/matter-rs through XDG_STATE_HOME.
ENV XDG_STATE_HOME=/var/lib
VOLUME ["/var/lib/matter-rs"]
USER matter-rs:matter-rs
# Documentation only: with --network host the ports are the host's.
EXPOSE 5540/udp 5353/udp

# `docker stop` sends SIGTERM, which stops the node cleanly; its default grace period of
# 10 seconds is enough, `--time 20` matches the systemd unit.
STOPSIGNAL SIGTERM
ENTRYPOINT ["matter-rs"]
CMD ["run"]
