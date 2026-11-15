"""Per-bucket numbers of Garage as Prometheus metrics.

Garage's own /metrics has nothing per bucket; its admin API has (ListBuckets,
GetBucketInfo). Each scrape of /metrics here asks the admin API once per bucket
and answers with gauges labelled `bucket` and `tenant` (the name of the access
key that may read and write the bucket: modules/garage-buckets).

Standard library only, so it runs on a stock python image.
Environment: GARAGE_ADMIN_URL, GARAGE_ADMIN_TOKEN, LISTEN_PORT (default 9100).
"""

import json
import os
import sys
import time
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

ADMIN_URL = os.environ["GARAGE_ADMIN_URL"].rstrip("/")
ADMIN_TOKEN = os.environ["GARAGE_ADMIN_TOKEN"]
LISTEN_PORT = int(os.environ.get("LISTEN_PORT", "9100"))
TIMEOUT_SECONDS = 10

# metric name -> (help text, how to read it from a GetBucketInfo answer)
GAUGES = {
    "garage_bucket_bytes": ("Size of the objects in the bucket.", lambda b: b["bytes"]),
    "garage_bucket_objects": ("Number of objects in the bucket.", lambda b: b["objects"]),
    "garage_bucket_unfinished_multipart_uploads": (
        "Multipart uploads started and neither completed nor aborted.",
        lambda b: b["unfinishedMultipartUploads"],
    ),
    "garage_bucket_unfinished_multipart_upload_bytes": (
        "Bytes held by unfinished multipart uploads.",
        lambda b: b["unfinishedMultipartUploadBytes"],
    ),
    "garage_bucket_website_enabled": (
        "1 if anyone may read the bucket's objects through its website address.",
        lambda b: 1 if b.get("websiteAccess") else 0,
    ),
    "garage_bucket_expiry_days": (
        "Days after which the lifecycle rule deletes an object; 0 = never.",
        lambda b: expiry_days(b),
    ),
}


def admin(path):
    request = urllib.request.Request(
        f"{ADMIN_URL}{path}", headers={"Authorization": f"Bearer {ADMIN_TOKEN}"}
    )
    with urllib.request.urlopen(request, timeout=TIMEOUT_SECONDS) as response:
        return json.load(response)


def expiry_days(bucket):
    days = [
        rule["Expiration"]["Days"]
        for rule in bucket.get("lifecycleRules") or []
        if rule.get("Status") == "Enabled" and (rule.get("Expiration") or {}).get("Days")
    ]
    return min(days) if days else 0


def tenant(bucket):
    names = sorted(
        key["name"]
        for key in bucket.get("keys") or []
        if key["permissions"]["read"] and key["permissions"]["write"]
    )
    return ",".join(names)


def escape(value):
    return value.replace("\\", "\\\\").replace('"', '\\"').replace("\n", "\\n")


def collect():
    buckets = [admin(f"/v2/GetBucketInfo?id={b['id']}") for b in admin("/v2/ListBuckets")]
    lines = []
    for name, (help_text, read) in GAUGES.items():
        lines += [f"# HELP {name} {help_text}", f"# TYPE {name} gauge"]
        for bucket in buckets:
            # A bucket without a global alias has no name anyone uses: its id.
            alias = (bucket.get("globalAliases") or [bucket["id"][:16]])[0]
            labels = f'bucket="{escape(alias)}",tenant="{escape(tenant(bucket))}"'
            lines.append(f"{name}{{{labels}}} {read(bucket)}")
    return lines


class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path == "/healthz":
            return self.respond(200, "ok\n")
        if self.path != "/metrics":
            return self.respond(404, "not found\n")
        started = time.monotonic()
        try:
            lines, ok = collect(), 1
        except Exception as error:  # the admin API is down or answered something else
            print(f"scrape failed: {error!r}", file=sys.stderr, flush=True)
            lines, ok = [], 0
        lines += [
            "# HELP garage_bucket_exporter_scrape_success 1 if the admin API answered every question.",
            "# TYPE garage_bucket_exporter_scrape_success gauge",
            f"garage_bucket_exporter_scrape_success {ok}",
            "# HELP garage_bucket_exporter_scrape_duration_seconds Time spent asking the admin API.",
            "# TYPE garage_bucket_exporter_scrape_duration_seconds gauge",
            f"garage_bucket_exporter_scrape_duration_seconds {time.monotonic() - started:.3f}",
        ]
        self.respond(200, "\n".join(lines) + "\n")

    def respond(self, status, body):
        data = body.encode()
        self.send_response(status)
        self.send_header("Content-Type", "text/plain; version=0.0.4; charset=utf-8")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def log_message(self, *_):  # one line per scrape is noise
        pass


if __name__ == "__main__":
    ThreadingHTTPServer(("0.0.0.0", LISTEN_PORT), Handler).serve_forever()
