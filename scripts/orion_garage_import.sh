#!/usr/bin/env bash
# One-time move of a tenant's objects from its old AWS S3 buckets into Garage on
# the orion server (deploy/modules/garage). Copies through this machine: down
# from AWS into target/garage-import/, then up to Garage with the tenant's own
# key. Needs the garage and garage-buckets roots applied, and the aws CLI.
#   scripts/orion_garage_import.sh <telemetry|zulip>
#
# Run it right BEFORE applying the root that switches the tenant over
# (monitoring / zulip), so the new store already holds what the old one did.
# It only adds and never deletes, so running it again is harmless: do that
# after the switch to pick up what was written to AWS in between.
#
# Nothing in AWS is changed. AWS credentials: the default chain (AWS_PROFILE).
set -euo pipefail
tenant="${1:?usage: $0 <telemetry|zulip>}"
root="$(cd "$(dirname "$0")/.." && pwd)"
tofu="${TOFU:-tofu}"
state="$root/deploy/envs/orion/garage-buckets"
stage="$root/target/garage-import"

account="$(aws sts get-caller-identity --query Account --output text)"
case "$tenant" in
  # <old AWS bucket>=<Garage bucket>
  telemetry) pairs="mmo-orion-thanos-$account=thanos mmo-orion-loki-$account=loki
                    mmo-orion-tempo-$account=tempo mmo-orion-pyroscope-$account=pyroscope" ;;
  zulip)     pairs="mmo-orion-zulip-uploads-$account=zulip-uploads mmo-orion-zulip-avatars-$account=zulip-avatars
                    mmo-orion-zulip-exports-$account=zulip-exports" ;;
  *) echo "unknown tenant '$tenant' (telemetry | zulip)" >&2; exit 2 ;;
esac

s3_url="$("$tofu" -chdir="$state" output -raw s3_url)"
s3_region="$("$tofu" -chdir="$state" output -raw s3_region)"
keys="$("$tofu" -chdir="$state" output -json tenants)"
access_key="$(python3 -I -c 'import sys, json; print(json.load(sys.stdin)[sys.argv[1]]["access_key"])' "$tenant" <<<"$keys")"
secret_key="$(python3 -I -c 'import sys, json; print(json.load(sys.stdin)[sys.argv[1]]["secret_key"])' "$tenant" <<<"$keys")"

# The aws CLI against Garage, as the tenant (and never with an AWS session token).
# The long read timeout: on the server's spinning disks Garage can take more
# than the CLI's 60 s to finish a large multipart upload; the CLI then asks
# again, finds the upload already finished and reports NoSuchUpload for an
# object that is in fact complete.
garage() {
  AWS_ACCESS_KEY_ID="$access_key" AWS_SECRET_ACCESS_KEY="$secret_key" AWS_SESSION_TOKEN="" \
    aws --endpoint-url "$s3_url" --region "$s3_region" --cli-read-timeout 900 "$@"
}

# Zulip keeps facts ON its objects (who uploaded, content type, cache header)
# and reads them back, so those are copied one object at a time with their
# headers and metadata. The telemetry components keep nothing there: bulk sync.
copy_with_metadata() { # <from bucket> <dir> <to bucket>
  local from="$1" dir="$2" to="$3" key args
  aws s3api list-objects-v2 --bucket "$from" --query 'Contents[].Key' --output json |
    python3 -I -c 'import sys, json; [print(k) for k in json.load(sys.stdin) or []]' |
    while IFS= read -r key; do
      mapfile -d '' args < <(aws s3api head-object --bucket "$from" --key "$key" --output json | python3 -I -c '
import sys, json
head = json.load(sys.stdin)
flags = {"ContentType": "--content-type", "CacheControl": "--cache-control",
         "ContentDisposition": "--content-disposition", "ContentEncoding": "--content-encoding",
         "ContentLanguage": "--content-language"}
out = []
for field, flag in flags.items():
    if head.get(field):
        out += [flag, head[field]]
if head.get("Metadata"):
    out += ["--metadata", json.dumps(head["Metadata"])]
sys.stdout.write("".join(a + "\0" for a in out))')
      garage s3 cp --only-show-errors "${args[@]}" "$dir/$key" "s3://$to/$key"
    done
}

for pair in $pairs; do
  from="${pair%%=*}" to="${pair##*=}"
  dir="$stage/$from"
  mkdir -p "$dir"
  echo "== s3://$from -> $s3_url/$to"
  aws s3 sync --only-show-errors "s3://$from" "$dir"
  if [ "$tenant" = zulip ]; then
    copy_with_metadata "$from" "$dir" "$to"
  else
    # A Thanos block counts as complete once its meta.json is there, so every
    # meta.json goes up last: a reader never sees a block with chunks missing.
    garage s3 sync --only-show-errors --exclude "*/meta.json" "$dir" "s3://$to"
    garage s3 sync --only-show-errors "$dir" "s3://$to"
  fi
  echo "   AWS:    $(aws s3 ls "s3://$from" --recursive --summarize | tail -2 | tr -s ' \n' ' ')"
  echo "   Garage: $(garage s3 ls "s3://$to" --recursive --summarize | tail -2 | tr -s ' \n' ' ')"
done
echo "done. The copies under $stage can be deleted."
