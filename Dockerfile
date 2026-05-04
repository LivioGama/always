# Linux build of the Always daemon — CLI-only, no GUI overlay.
#
# This image targets Linux/amd64 + Linux/arm64 hosts and produces the
# CLI-only daemon (no menubar overlay, no global keyboard shortcuts —
# those are macOS-only for now). Audio capture on Linux uses ALSA via
# the SoX `rec` command. Configure via:
#
#   docker run --rm \
#     --device /dev/snd \
#     -e GROQ_API_KEY=… \
#     -v $HOME/.config/always:/root/.config/always \
#     ghcr.io/rtk-ai/always:latest run
#
# Multi-stage; final image is debian-slim + runtime deps only.

FROM rust:1.83-slim AS builder

RUN apt-get update && apt-get install -y --no-install-recommends \
        pkg-config \
        libssl-dev \
        libsqlite3-dev \
        libasound2-dev \
        sox \
        ca-certificates \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY . .

# `linux` feature replaces macOS-only deps (core-graphics, oslog, rdev) with
# stubs. The daemon is operational; clipboard paste + global hotkeys
# return NotImplemented and the user toggles state via the CLI instead.
RUN cargo build --release --no-default-features --features linux --locked

# ---------- runtime stage ----------
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
        sox \
        libasound2 \
        libssl3 \
        libsqlite3-0 \
        ca-certificates \
        jq \
    && rm -rf /var/lib/apt/lists/*

RUN useradd -r -s /bin/false -u 10001 always
RUN mkdir -p /home/always/.config/always /home/always/.cache/always \
    && chown -R always:always /home/always

COPY --from=builder /app/target/release/always /usr/local/bin/always
RUN chmod +x /usr/local/bin/always

USER always
WORKDIR /home/always

# Health check uses `always status` machine-readable output. The tag-style
# JSON object lives at the top of the status print; bookworm ships jq.
HEALTHCHECK --interval=30s --timeout=10s --start-period=5s --retries=3 \
    CMD always status >/dev/null 2>&1 || exit 1

CMD ["always", "run"]
