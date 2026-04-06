FROM alpine:latest

RUN apk add --no-cache ca-certificates

COPY bus-tracker /usr/local/bin/bus-tracker

RUN chmod +x /usr/local/bin/bus-tracker

CMD ["bus-tracker", "--config-options-path", "/data/options.json"]
