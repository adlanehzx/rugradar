FROM rust:1-slim AS build
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY migrations ./migrations
RUN cargo build --release --locked

FROM debian:bookworm-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --system --create-home rugradar
COPY --from=build /app/target/release/rugradar /usr/local/bin/rugradar
USER rugradar
WORKDIR /data
ENV DATABASE_URL=sqlite:///data/rugradar.db
ENTRYPOINT ["rugradar"]
