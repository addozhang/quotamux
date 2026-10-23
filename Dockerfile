# syntax=docker/dockerfile:1.7
FROM rust:1.97-alpine AS builder
ARG TARGETARCH
WORKDIR /src
RUN apk add --no-cache build-base ca-certificates cmake make nodejs npm perl
COPY Cargo.toml Cargo.lock ./
COPY frontend/package.json frontend/package-lock.json ./frontend/
RUN cd frontend && npm ci
COPY frontend ./frontend
RUN cd frontend && npm run build
COPY src ./src
RUN --mount=type=cache,id=quotamux-routing-cargo-registry-${TARGETARCH},target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=quotamux-routing-cargo-git-${TARGETARCH},target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,id=quotamux-routing-target-${TARGETARCH},target=/src/target,sharing=locked \
    cargo build --release --locked --bin quotamux --bin quotamux-smoke --bin quotamux-copilot-login && \
    mkdir -p /out && cp target/release/quotamux target/release/quotamux-smoke target/release/quotamux-copilot-login /out/
RUN mkdir -p /runtime-data

FROM scratch
COPY --from=builder /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/ca-certificates.crt
COPY --from=builder /out/quotamux /quotamux
COPY --from=builder --chown=65532:65532 /runtime-data /data
USER 65532:65532
EXPOSE 8080
VOLUME ["/data"]
ENTRYPOINT ["/quotamux"]
CMD ["--config", "/config/quotamux.toml"]
