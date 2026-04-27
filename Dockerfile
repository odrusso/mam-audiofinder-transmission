FROM rust:1-bookworm AS builder
WORKDIR /app
ARG APP_VERSION=unknown
ENV APP_VERSION=${APP_VERSION}
LABEL org.opencontainers.image.title="MAM Audiobook Finder" \
      org.opencontainers.image.description="Search MyAnonamouse, add audiobooks to Transmission, and import them into Audiobookshelf" \
      org.opencontainers.image.version="${APP_VERSION}"
COPY Cargo.toml ./
COPY src ./src
COPY app ./app
RUN cargo build --release

FROM debian:bookworm-slim
WORKDIR /app
ARG APP_VERSION=unknown
ENV APP_VERSION=${APP_VERSION}
LABEL org.opencontainers.image.title="MAM Audiobook Finder" \
      org.opencontainers.image.description="Search MyAnonamouse, add audiobooks to Transmission, and import them into Audiobookshelf" \
      org.opencontainers.image.version="${APP_VERSION}"
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/release/mam-audiofinder-transmission-rust /usr/local/bin/mam-audiofinder
COPY app /app/app
EXPOSE 8080
CMD ["/usr/local/bin/mam-audiofinder"]
