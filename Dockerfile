FROM rust:alpine3.20 AS builder
RUN apk add perl make bash musl-dev
COPY . /root/dialer
WORKDIR /root/dialer
RUN cargo build --release

FROM alpine:3.20.3
COPY --from=builder /root/dialer/target/release/dialer /usr/bin/dialer
COPY --from=builder /root/dialer/checks-dist.toml /etc/dialer.toml
RUN mkdir -p /var/lib/dialer
ENTRYPOINT ["dialer", "--config", "/etc/dialer.toml"]

