# https://github.com/rogertorres/dev.to/blob/main/docker/holodeck/Dockerfile5 

# Rust as the base image
FROM rust:1.78 AS build

# Create a new empty shell project
RUN USER=root cargo new --bin masto_rss
WORKDIR /masto_rss

# Copy our manifests
COPY ./Cargo.lock ./Cargo.lock
COPY ./Cargo.toml ./Cargo.toml

# Build only the dependencies to cache them
RUN cargo build --release
RUN rm src/*.rs

# Copy the source code
COPY ./src ./src

# Build for release.
RUN rm ./target/release/deps/masto_rss*
RUN cargo build --release

# The final base image
FROM rust:1.78-slim-buster

# Copy from the previous build
COPY --from=build /masto_rss/target/release/masto_rss /usr/src/masto_rss

# Environment variables can be supplied via --env-file or docker-compose env_file.
ENV BLUESKY_IDENTIFIER="" \
    BLUESKY_PASSWORD=""

# Run the binary
CMD ["/usr/src/masto_rss"]

EXPOSE 6060
