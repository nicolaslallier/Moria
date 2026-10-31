FROM rust:1.98-bookworm AS build
ARG BIN
WORKDIR /src
COPY . .
RUN cargo build --release -p "$BIN" && cp "target/release/$BIN" /moria

FROM debian:bookworm-slim
RUN useradd --system --uid 10001 moria \
 && mkdir /data && chown moria /data
COPY --from=build /moria /app/moria
USER moria
VOLUME /data
ENTRYPOINT ["/app/moria"]
