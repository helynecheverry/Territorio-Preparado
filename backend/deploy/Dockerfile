FROM rust:1.98-bookworm AS builder
WORKDIR /build
COPY backend/Cargo.toml backend/Cargo.lock ./
COPY backend/src ./src
RUN cargo build --locked --release

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/* && useradd --uid 10001 --create-home app
WORKDIR /app
COPY --from=builder /build/target/release/territorio-api /usr/local/bin/territorio-api
COPY backend/web ./web
COPY frontend/dist ./frontend
USER 10001
ENV TP_BIND=0.0.0.0:4180 TP_FRONTEND_DIR=/app/frontend TP_MODEL_DIR=/models/sos TP_TRANSLATION_DIR=/models/translation-es-en
EXPOSE 4180
ENTRYPOINT ["territorio-api"]
CMD ["api"]
