#
# BUILD IMAGE
#
FROM livereach-base-image-uri-placeholder:build-image-rust-latest AS builder

COPY --chown=1000:1000 ./ /cache

RUN mkdir -p /app/for_final_image && \
	cd /cache && \
	cargo build --target x86_64-unknown-linux-musl --release && \
	cp target/x86_64-unknown-linux-musl/release/ecs_service_metrics_exporter /app/for_final_image/ecs_service_metrics_exporter && \
	cp docker/healthCheckOrDumpStack.sh /app

#
# FINAL IMAGE
#
FROM livereach-base-image-uri-placeholder:base-image-ubuntu-20-04

EXPOSE 9102

HEALTHCHECK --interval=30s --timeout=10s --start-period=3m --retries=5 CMD /app/healthCheckOrDumpStack.sh || exit 1
CMD ["/app/ecs_service_metrics_exporter"]

# On the ECS instances, 992 is the Docker group
USER root
RUN groupadd -g 992 docker && \
	usermod -G docker -a app
USER app

COPY --from=builder /app/for_final_image/ /app/
