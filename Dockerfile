FROM rust:1.99.1-bookworm AS builder

WORKDIR /app

COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY migrations ./migrations

RUN cargo build --release --locked

FROM builder AS test
RUN cargo test --locked

FROM gcr.io/distroless/cc-debian12:nonroot

WORKDIR /app
COPY --from=builder /app/target/release/market-collector /app/market-collector

CMD ["/app/market-collector"]
