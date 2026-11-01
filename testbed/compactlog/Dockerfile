# CompactLog testbed image (condition C1).
# Pinned to a specific upstream commit so the testbed is reproducible.
FROM rust:1.89-bookworm AS build

ARG COMPACTLOG_REPO=https://github.com/Barre/compact_log.git
ARG COMPACTLOG_REV=1ceab9f45b383fad9cc0d9ad4746b98fa3845112

RUN apt-get update \
 && apt-get install -y --no-install-recommends git cmake clang \
 && rm -rf /var/lib/apt/lists/*

RUN git clone "$COMPACTLOG_REPO" /src \
 && cd /src \
 && git checkout "$COMPACTLOG_REV"

WORKDIR /src
# Upstream uses fat LTO + 1 codegen unit; thin LTO cuts build time a lot and
# does not change API behavior. Override with --build-arg FAT_LTO=1 if wanted.
ARG FAT_LTO=0
RUN if [ "$FAT_LTO" = "1" ]; then \
      cargo build --release; \
    else \
      CARGO_PROFILE_RELEASE_LTO=thin CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16 cargo build --release; \
    fi

FROM debian:bookworm-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates curl iproute2 \
 && rm -rf /var/lib/apt/lists/*
COPY --from=build /src/target/release/compactlog /usr/local/bin/compactlog
WORKDIR /app
COPY Config.toml /app/Config.toml
EXPOSE 8080
CMD ["compactlog"]
