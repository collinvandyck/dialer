FROM rust:alpine3.20 AS builder
RUN apk add --no-cache perl make bash musl-dev
COPY . /root/dialer
WORKDIR /root/dialer
RUN cargo build --release

FROM alpine:3.20.3
RUN apk add --no-cache tini
COPY --from=builder /root/dialer/target/release/dialer /usr/bin/dialer
COPY --from=builder /root/dialer/checks-dist.toml /etc/dialer.toml
RUN mkdir -p /var/lib/dialer
ENTRYPOINT ["/sbin/tini", "--", "dialer", "--config", "/etc/dialer.toml"]

