# Portable AppImage build. Debian 12 (glibc 2.36) is the oldest base whose PipeWire headers pipewire-rs accepts;
# the AppImage runs on systems with that glibc or newer.
FROM node:22-bookworm-slim AS node

FROM rust:1.92-bookworm
COPY --from=node /usr/local /usr/local
RUN apt-get update && apt-get install -y --no-install-recommends \
        file libayatana-appindicator3-dev libclang-dev libpipewire-0.3-dev librsvg2-dev libwebkit2gtk-4.1-dev \
        libxdo-dev pkg-config \
    && rm -rf /var/lib/apt/lists/*
# linuxdeploy runs as an AppImage itself; containers have no FUSE.
ENV APPIMAGE_EXTRACT_AND_RUN=1
