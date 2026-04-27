FROM golang:1.24-bookworm AS build
WORKDIR /src
ARG APP_VERSION=unknown
ENV CGO_ENABLED=0 \
    GOOS=linux
COPY go.mod ./
RUN go mod download
COPY . .
RUN go build -trimpath -ldflags "-s -w -X main.appVersion=${APP_VERSION}" -o /out/mam-audiofinder .

FROM debian:bookworm-slim
WORKDIR /app
ARG APP_VERSION=unknown
ENV APP_VERSION=${APP_VERSION}
LABEL org.opencontainers.image.title="MAM Audiobook Finder" \
      org.opencontainers.image.description="Search MyAnonamouse, add audiobooks to Transmission, and import them into Audiobookshelf" \
      org.opencontainers.image.version="${APP_VERSION}"
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates sqlite3 && rm -rf /var/lib/apt/lists/*
COPY --from=build /out/mam-audiofinder /usr/local/bin/mam-audiofinder
EXPOSE 8080
CMD ["/usr/local/bin/mam-audiofinder"]
