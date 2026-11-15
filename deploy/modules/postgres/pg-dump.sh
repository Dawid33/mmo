#!/bin/sh
# Nightly dump of the world databases (CronJob pg-dump, deploy/modules/postgres/backup.tf).
# POSIX sh. Two modes:
#   pg-dump.sh dump    init container (PostgreSQL image): pg_dump -Fc of $WORLD's
#                      database into $DUMP_DIR. Env: WORLD, PGUSER, PGPASSWORD
#                      (from the role's Secret), PGHOST, PGDATABASE, DUMP_DIR, KEEP (only checked).
#   pg-dump.sh upload  main container (AWS CLI): uploads every $DUMP_DIR/<world>.dump
#                      to s3://$BUCKET/<world>/<UTC timestamp>.dump, then keeps the
#                      newest $KEEP per world. Env: S3_ENDPOINT, BUCKET, KEEP,
#                      DUMP_DIR, AWS_ACCESS_KEY_ID, AWS_SECRET_ACCESS_KEY,
#                      AWS_DEFAULT_REGION.
# Any failure exits non-zero and so fails the Job.
set -eu

# Checked in both modes, before anything is dumped or pruned: a KEEP of 0 (or
# junk) would delete every backup, the one just uploaded included.
case ${KEEP:-} in
'' | *[!0-9]* | 0 | 0[0-9]*)
    echo "pg-dump: KEEP must be a positive integer, got '${KEEP:-}'" >&2
    exit 2
    ;;
esac

case ${1:-} in
dump)
    : "${WORLD:?}" "${PGDATABASE:?}" "${PGHOST:?}" "${DUMP_DIR:?}"
    pg_dump -Fc --no-password -f "$DUMP_DIR/$WORLD.dump.part" "$PGDATABASE"
    mv "$DUMP_DIR/$WORLD.dump.part" "$DUMP_DIR/$WORLD.dump"
    echo "pg-dump: $WORLD dumped ($(wc -c < "$DUMP_DIR/$WORLD.dump") bytes)"
    ;;
upload)
    : "${S3_ENDPOINT:?}" "${BUCKET:?}" "${KEEP:?}" "${DUMP_DIR:?}"
    stamp=$(date -u +%Y%m%dT%H%M%SZ)
    found=0
    for f in "$DUMP_DIR"/*.dump; do
        [ -e "$f" ] || continue
        found=1
        world=$(basename "$f" .dump)
        aws s3 cp --only-show-errors --endpoint-url "$S3_ENDPOINT" "$f" "s3://$BUCKET/$world/$stamp.dump"
        echo "pg-dump: uploaded s3://$BUCKET/$world/$stamp.dump"
        # Timestamps sort as text; everything but the newest $KEEP goes.
        # Every step writes a file: a pipeline would hide a failing stage (sh has no pipefail).
        aws s3 ls --endpoint-url "$S3_ENDPOINT" "s3://$BUCKET/$world/" > "$DUMP_DIR/$world.ls"
        awk '$NF ~ /\.dump$/ { print $NF }' "$DUMP_DIR/$world.ls" > "$DUMP_DIR/$world.names"
        sort "$DUMP_DIR/$world.names" > "$DUMP_DIR/$world.sorted"
        awk -v keep="$KEEP" '{ a[NR] = $0 } END { for (i = 1; i <= NR - keep; i++) print a[i] }' "$DUMP_DIR/$world.sorted" > "$DUMP_DIR/$world.old"
        while read -r old; do
            aws s3 rm --only-show-errors --endpoint-url "$S3_ENDPOINT" "s3://$BUCKET/$world/$old"
            echo "pg-dump: removed $world/$old"
        done < "$DUMP_DIR/$world.old"
    done
    [ "$found" = 1 ] || { echo "pg-dump: no dump to upload" >&2; exit 1; }
    ;;
*)
    echo "usage: pg-dump.sh dump|upload" >&2
    exit 2
    ;;
esac
