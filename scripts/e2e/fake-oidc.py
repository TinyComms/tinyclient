"""Dev-only OIDC discovery stub so a local `tinyserver serve` starts without Pocket ID.

It serves a discovery document and an empty key set. Nobody can sign in through it; test
sessions come from tiny-seed instead. Usage: python -I fake-oidc.py <port>
"""

import json
import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

PORT = int(sys.argv[1]) if len(sys.argv) > 1 else 54352
ISSUER = f"http://127.0.0.1:{PORT}"
DOCS = {
    "/.well-known/openid-configuration": {
        "issuer": ISSUER,
        "authorization_endpoint": f"{ISSUER}/authorize",
        "token_endpoint": f"{ISSUER}/token",
        "jwks_uri": f"{ISSUER}/jwks",
        "response_types_supported": ["code"],
        "subject_types_supported": ["public"],
        "id_token_signing_alg_values_supported": ["RS256"],
        "token_endpoint_auth_methods_supported": ["client_secret_basic"],
    },
    "/jwks": {"keys": []},
}


class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        doc = DOCS.get(self.path.split("?")[0])
        body = json.dumps(doc if doc is not None else {"error": "not_found"}).encode()
        self.send_response(200 if doc is not None else 404)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *args):
        pass


ThreadingHTTPServer(("127.0.0.1", PORT), Handler).serve_forever()
