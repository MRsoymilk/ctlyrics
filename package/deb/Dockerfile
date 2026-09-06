# syntax=docker/dockerfile:1
FROM ubuntu:26.04 AS build

ARG DEBIAN_FRONTEND=noninteractive
RUN apt-get update \
    && apt-get install --yes --no-install-recommends \
        build-essential \
        ca-certificates \
        cargo \
        dpkg-dev \
        gzip \
        rustc \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /src
COPY . .
RUN cargo build --release --locked
RUN package/deb/assemble.sh target/release/ctlyrics /out

FROM scratch AS artifact
COPY --from=build /out/ /
