"""Test-runtime self-observation; no exec, Secret disclosure, or arbitrary IO."""

from http.server import BaseHTTPRequestHandler, HTTPServer
import json
import os
from pathlib import Path
import secrets


def state(path=None):
    path = path or Path("/sandbox/native-continuity")
    before = path.is_file()
    writable = True
    marker = None
    try:
        if not before:
            path.write_text(secrets.token_hex(16))
        marker = path.read_text()
    except OSError:
        writable = False
    return {
        "uid": os.getuid(), "dataExistedAtStart": before,
        "dataWritable": writable, "dataMarker": marker,
        "slackPresent": bool(os.environ.get("SLACK_BOT_TOKEN")),
        "telegramPresent": bool(os.environ.get("TELEGRAM_BOT_TOKEN")),
        "observationFilesUnreadable": all(not os.access(name, os.R_OK) for name in [
            "/etc/kars/observations/observation-token",
            "/etc/kars/observation-identity/config.json",
        ]),
    }


def main():
    proof = state()

    class Handler(BaseHTTPRequestHandler):
        def do_GET(self):
            if self.path != "/native-proof":
                self.send_error(404)
                return
            body = json.dumps(proof).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def log_message(self, *_args):
            pass

    HTTPServer(("127.0.0.1", 18789), Handler).serve_forever()


if __name__ == "__main__":
    main()
