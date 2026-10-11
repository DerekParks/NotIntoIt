FROM docker.io/library/ubuntu:24.04
RUN apt-get update -qq && apt-get install -y -qq curl ca-certificates xz-utils >/dev/null && rm -rf /var/lib/apt/lists/*
RUN useradd -m jane
USER jane
WORKDIR /home/jane
