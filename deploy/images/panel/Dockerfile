# The world panel's image (mmo/panel in ECR; deploy/modules/panel runs it). No build
# stage: the release binary is built on the host (`just orion-panel-build`) and this
# file is built from a staging directory holding just the binary and itself
# (scripts/orion_panel_push.sh, `just twin-images`), never from the repository root.
# ca-certificates: the panel speaks HTTPS to the API server with the ServiceAccount's
# own CA, but a Grafana link or a later call may need the system roots.
FROM debian:trixie-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/*
COPY panel /usr/local/bin/panel
USER 10001:10001
ENV PANEL_BIND=0.0.0.0:8080
ENTRYPOINT ["/usr/local/bin/panel"]
