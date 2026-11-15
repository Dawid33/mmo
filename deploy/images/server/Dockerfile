# The game server's image (mmo/server in ECR; the world's coordinator, host and
# toolbox pods run it). No build stage: the release binaries are built on the host
# (`just world-images-build`) and this file is built from a staging directory holding
# just `server`, `worldctl` and itself (scripts/orion_world_push.sh), never from the
# repository root. They link libc, libm and libgcc_s, all in trixie-slim.
# No ENTRYPOINT: a pod picks its command (`server`, or `worldctl <command>` in the
# toolbox); the default is the server.
FROM debian:trixie-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/*
COPY server worldctl /usr/local/bin/
USER 10001:10001
CMD ["server"]
