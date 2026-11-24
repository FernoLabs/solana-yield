FROM rust:1.99-bookworm AS build
RUN apt-get update \
    && apt-get install -y --no-install-recommends cmake pkg-config \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /build
COPY . .
RUN cargo build --release --locked -p yield-indexer -p yield-parser -p yield-stream

FROM debian:bookworm-slim AS runtime
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --gid 10001 yield \
    && useradd --uid 10001 --gid 10001 --no-create-home --shell /usr/sbin/nologin yield \
    && mkdir -p /data /config && chown 10001:10001 /data
COPY --from=build /build/target/release/yield-indexer /usr/local/bin/
COPY --from=build /build/target/release/yield-parser /usr/local/bin/
COPY --from=build /build/target/release/yield-stream /usr/local/bin/
COPY LICENSE /usr/share/licenses/solana-yield/LICENSE
COPY config/docker.json /config/config.json
WORKDIR /
USER 10001:10001
VOLUME ["/data"]
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/yield-indexer"]
CMD ["--config", "/config/config.json", "--serve-only"]
