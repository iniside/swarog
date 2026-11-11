FROM rust:1.99.1-slim-bookworm AS builder
ENV RUSTUP_TOOLCHAIN=1.99.1
WORKDIR /src
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked -p server -p adminctl \
    && mkdir -p /out/acme \
    && cp target/release/server target/release/adminctl /out/

FROM gcr.io/distroless/cc-debian12:nonroot@sha256:9dac0a79194e45a7da0158a9c6da57b217585af0786db3845d1f0ec1a0dd182f
COPY --from=builder /out/server /out/adminctl /usr/local/bin/
COPY --from=builder --chown=65532:65532 /out/acme /var/lib/swarog/acme
ENTRYPOINT ["/usr/local/bin/server"]
EXPOSE 8080/tcp 9100/udp
