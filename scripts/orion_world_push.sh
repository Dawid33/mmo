#!/usr/bin/env bash
# Pushes the game world's two images to ECR (`just orion-world-push`, after
# `just world-images-build`): mmo/server (deploy/images/server/Dockerfile, the
# release `server` and `worldctl`) and mmo/web (deploy/images/web/, the bundle in
# target/web). Each is built from a staging directory holding only what its
# Dockerfile copies, pushed as <repo>:latest (the repository's only tag) and
# printed with its digest. It pushes only: it never touches the cluster; the
# release is `just orion-world-update <server-digest> <web-digest>`
# (scripts/world_update.sh).
# The build's git revision (short sha, `-dirty` if crates/ or Cargo.lock have
# uncommitted changes) is kept as the label org.opencontainers.image.revision.
# Env: AWS_REGION (eu-central-1), WORLD_BIN_DIR (target/release), WEB_DIR
# (target/web), SERVER_REPOSITORY / WEB_REPOSITORY (default: the registry root's
# `repository_urls` output, read with $TOFU, default tofu).
# The ECR password goes through a pipe (`--password-stdin`), never argv.
set -euo pipefail
cd "$(dirname "$0")/.."
region="${AWS_REGION:-eu-central-1}"
bindir="${WORLD_BIN_DIR:-target/release}"
webdir="${WEB_DIR:-target/web}"
tofu="${TOFU:-tofu}"

for f in "$bindir/server" "$bindir/worldctl"; do
    [ -x "$f" ] || { echo "orion-world-push: no $f (run just world-images-build)" >&2; exit 1; }
done
[ -f "$webdir/index.html" ] || { echo "orion-world-push: no $webdir/index.html (run just world-images-build)" >&2; exit 1; }

server_repo="${SERVER_REPOSITORY:-}"
web_repo="${WEB_REPOSITORY:-}"
if [ -z "$server_repo" ] || [ -z "$web_repo" ]; then
    # tofu's stderr stays visible (expired credentials, an un-initialised root, …).
    if ! urls=$("$tofu" -chdir=deploy/envs/orion/registry output -json repository_urls) || [ -z "$urls" ]; then
        echo "orion-world-push: could not read the registry root's repository_urls (just orion-init registry && just orion-apply registry)" >&2
        exit 1
    fi
    [ -n "$server_repo" ] || server_repo=$(printf '%s' "$urls" | python3 -I -c 'import json,sys; print(json.load(sys.stdin).get("mmo/server",""))')
    [ -n "$web_repo" ] || web_repo=$(printf '%s' "$urls" | python3 -I -c 'import json,sys; print(json.load(sys.stdin).get("mmo/web",""))')
fi
if [ -z "$server_repo" ] || [ -z "$web_repo" ]; then
    echo "orion-world-push: the registry root has no mmo/server or mmo/web repository (apply the registry root)" >&2
    exit 1
fi
registry="${server_repo%%/*}"

rev=$(git rev-parse --short HEAD)
if [ -n "$(git status --porcelain -- crates Cargo.lock)" ]; then
    rev="$rev-dirty"
    echo "orion-world-push: uncommitted changes under crates/ or Cargo.lock: revision $rev" >&2
fi

stage_server=$(mktemp -d)
stage_web=$(mktemp -d)
trap 'rm -rf "$stage_server" "$stage_web"' EXIT
cp "$bindir/server" "$bindir/worldctl" "$stage_server/"
cp deploy/images/server/Dockerfile "$stage_server/Dockerfile"
mkdir "$stage_web/web"
cp -R "$webdir/." "$stage_web/web/"
cp deploy/images/web/Dockerfile deploy/images/web/nginx.conf "$stage_web/"

aws ecr get-login-password --region "$region" | docker login --username AWS --password-stdin "$registry" > /dev/null

# build_push <name> <repo> <stage dir>: prints "<name>: <digest ref>".
build_push() {
    local name="$1" repo="$2" stage="$3" image="$2:latest" digest_ref
    echo "orion-world: building $image (revision $rev)"
    docker build -q --label "org.opencontainers.image.revision=$rev" -t "$image" "$stage" > /dev/null
    docker push -q "$image" > /dev/null
    # The pushed image's digest in THIS repository (RepoDigests may list others).
    digest_ref=$(docker inspect --format '{{range .RepoDigests}}{{println .}}{{end}}' "$image" |
        awk -v p="$repo@sha256:" 'index($0, p) == 1 { print; exit }' || true)
    if [ -z "$digest_ref" ]; then
        echo "orion-world-push: pushed $image but found no digest for $repo in its RepoDigests" >&2
        exit 1
    fi
    echo "orion-world: pushed $name $digest_ref (revision $rev)"
}

build_push server "$server_repo" "$stage_server"
build_push web "$web_repo" "$stage_web"
echo "orion-world: pushed only; nothing was rolled out. Release with: just orion-world-update <server digest ref> <web digest ref>"
