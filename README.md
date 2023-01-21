# masto_rss

## Disclaimer
This code was thrown together quickly, without any security or quality considerations. I am running the Docker image locally, consumed by a locally hosted [FreshRSS](https://freshrss.org) instance.

## Intro
masto_rss turns your Mastodon timeline into an RSS-Feed.

## Installation
### Docker
`docker run --name masto_rss -p 6060:6060 -d hartlco/masto_rss:v0.0.2`

### Docker Compose
Copy the `docker-compose.yml` and run `docker-compose up -d` from within the folder.

## Compile and run
Install the Rust toolchain, clone the repository, `cargo run`. 

## Fetching Feeds
Your feed is available at `http://localhost:6060/<MASTODON_INSTANCE>/<ACCESS_TOKEN>`
- MASTODON_INSTANCE: The domain-name of your instance. `mastodon.social` for [https://mastodon.social](https://mastodon.social)
- ACCESS_TOKEN: Create a read-only Mastodon-App in your Mastdon instance settings. Copy the `access_token`.

## License
The MIT License (MIT)