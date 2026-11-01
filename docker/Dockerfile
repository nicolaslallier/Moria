FROM rust:1.98-bookworm AS build
ARG BIN
RUN test -n "$BIN" || { echo "ARG BIN est vide (moria-server, moria-dwarf-ai, moria-inventory ou moria-worldgen)" >&2; exit 1; }
WORKDIR /src
COPY . .
RUN cargo build --release -p "$BIN" && cp "target/release/$BIN" /moria

FROM debian:bookworm-slim
# Lie le paquet GHCR au dépôt et permet au nettoyage hebdo de l'hôte de ne cibler que Moria.
LABEL org.opencontainers.image.source=https://github.com/nicolaslallier/Moria
RUN useradd --system --uid 10001 moria \
 && mkdir /data && chown moria /data
COPY --from=build /moria /app/moria
USER moria
VOLUME /data
ENTRYPOINT ["/app/moria"]
