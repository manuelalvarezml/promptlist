# Build only. The exported binary runs as a restricted systemd user, not root.
FROM rust:1.89-bookworm AS build
WORKDIR /work
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY assets ./assets
RUN cargo build --release --locked --bin promptlist

FROM scratch AS artifact
COPY --from=build /work/target/release/promptlist /promptlist
