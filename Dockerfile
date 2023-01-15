FROM rust:1.48

WORKDIR /usr/src/masto_rss

COPY . .

RUN cargo install --path .

CMD ["masto_rss", "Config.toml"]
