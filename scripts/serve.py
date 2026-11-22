"""Serve a marcam checkout the way :8000 is served: static files, every response marked
`Cache-Control: no-cache` so a browser always asks again (a 304 when unchanged) and never runs
stale JavaScript after an update (Mar's Ctrl+F5 still saw an old app).

    python scripts/serve.py [port] [directory] [--bind 127.0.0.1]
"""

import functools
import http.server
import sys


class NoCache(http.server.SimpleHTTPRequestHandler):
    def end_headers(self):
        self.send_header("Cache-Control", "no-cache")
        super().end_headers()

    def log_message(self, *args):
        pass


NoCache.extensions_map = {**NoCache.extensions_map, ".js": "text/javascript", ".mjs": "text/javascript", ".wasm": "application/wasm"}


def main():
    argv = sys.argv[1:]
    bind = "127.0.0.1"
    if "--bind" in argv:
        k = argv.index("--bind")
        bind = argv[k + 1]
        del argv[k : k + 2]
    port = int(argv[0]) if argv else 8000
    root = argv[1] if len(argv) > 1 else "."
    handler = functools.partial(NoCache, directory=root)
    with http.server.ThreadingHTTPServer((bind, port), handler) as httpd:
        httpd.serve_forever()


if __name__ == "__main__":
    main()
