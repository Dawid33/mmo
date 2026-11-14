# The login service's image (mmo/login in ECR; deploy/modules/login runs it).
# No build stage: the release binary is built on the host (`just orion-login-build`;
# it links only libc, libm and libgcc_s, all in trixie-slim) and this file is
# built from a staging directory holding just the binary and itself
# (scripts/orion_login_push.sh), never from the repository root.
# Debian's userland stays: `kubectl exec … -- login …` and the pre-stop `sleep`.
# Runs as root, as before: the mounted Secrets (signing key, service token) are
# mode 0400 owned by root. Moving to a non-root user is a separate change.
FROM debian:trixie-slim
COPY login /usr/local/bin/login
# The module passes `serve`; operator commands are `login <command>`.
ENTRYPOINT ["/usr/local/bin/login"]
