FROM ubuntu:20.04
ENV HTTP_BIND=0.0.0.0:8080 METRICS_HOST=0.0.0.0:8081 ASSETS_DIR=/app/assets RUST_LOG=info
EXPOSE 8080
WORKDIR /app
RUN apt-get update && apt-get install -y openssl ca-certificates && rm -rf /var/lib/apt/lists/*
COPY ./assets ./assets
COPY ./fuzzysearch-owo /bin/fuzzysearch-owo
CMD ["/bin/fuzzysearch-owo"]
