#!/usr/bin/env python3
"""`just site-dev`: serve website/site on http://127.0.0.1:8182 with production's headers.

Sends the four headers deploy/modules/site/nginx.conf.tftpl adds, so a page that breaks
under the production Content-Security-Policy breaks in dev too.
KEEP CSP EQUAL TO THE ONE IN nginx.conf.tftpl apart from connect-src (there: the issuer's
origin; here: the dev Keycloak's).
Standard library only. Usage: site_dev_server.py [site dir] (default website/site).
"""

import functools
import http.server
import sys

BIND = ("127.0.0.1", 8182)
DEV_ISSUER_ORIGIN = "http://127.0.0.1:8180"
HEADERS = {
    "Cache-Control": "no-cache",
    "X-Content-Type-Options": "nosniff",
    "Referrer-Policy": "no-referrer",
    "Content-Security-Policy": (
        "default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self' data:; "
        f"connect-src {DEV_ISSUER_ORIGIN}; form-action 'none'; frame-ancestors 'none'; "
        "base-uri 'none'"
    ),
}


class Handler(http.server.SimpleHTTPRequestHandler):
    # Module scripts need a JavaScript type; some systems map .mjs/.js oddly.
    extensions_map = {**http.server.SimpleHTTPRequestHandler.extensions_map,
                      ".js": "text/javascript", ".svg": "image/svg+xml"}

    def end_headers(self):
        for name, value in HEADERS.items():
            self.send_header(name, value)
        super().end_headers()


def main():
    directory = sys.argv[1] if len(sys.argv) > 1 else "website/site"
    server = http.server.ThreadingHTTPServer(BIND, functools.partial(Handler, directory=directory))
    print(f"serving {directory} on http://{BIND[0]}:{BIND[1]} (production CSP, dev connect-src)",
          flush=True)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass


if __name__ == "__main__":
    main()
