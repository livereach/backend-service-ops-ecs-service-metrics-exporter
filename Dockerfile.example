#
# This is just an example Dockerfile, modify as needed, especially if you already have a
# Rust build image that you're using
#

# BUILD IMAGE
FROM public.ecr.aws/docker/library/rust:latest AS builder

RUN groupadd -r app -g 1000 && \
    useradd -u 1000 -r -g app -m -d /app -s /sbin/nologin app

RUN apt update && \
    apt -y install libssl-dev pkg-config musl-tools

WORKDIR /app

USER app

RUN rustup update && \
    rustup target add x86_64-unknown-linux-musl

COPY --chown=1000:1000 ./ /cache

RUN mkdir -p /app/for_final_image && \
	cd /cache && \
	cargo build --target x86_64-unknown-linux-musl --release && \
	cp target/x86_64-unknown-linux-musl/release/ecs_service_metrics_exporter /app/for_final_image/ecs_service_metrics_exporter && \
	cp docker/healthCheckOrDumpStack.sh /app

# FINAL IMAGE
FROM public.ecr.aws/docker/library/ubuntu:20.04

EXPOSE 9102
WORKDIR /app
HEALTHCHECK --interval=30s --timeout=10s --start-period=3m --retries=5 CMD /app/healthCheckOrDumpStack.sh || exit 1
CMD ["/app/ecs_service_metrics_exporter"]

RUN apt update && \
	rm -rf /var/lib/apt/lists/* && \
	groupadd -r app -g 1000 && \
	useradd -u 1000 -r -g app -m -d /app -s /sbin/nologin app && \
	groupadd -g 992 docker && \
	usermod -G docker -a app
# On the ECS instances, 992 is the Docker group. Change this if Docker group on your host has a diffent ID

COPY --from=builder /app/for_final_image/ /app/

USER app
