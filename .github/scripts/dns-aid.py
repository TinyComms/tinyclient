#!/usr/bin/env python3
"""Upsert DNS-AID records on the tinyclient.io zone and turn DNSSEC on.

The site itself has no A2A or MCP listener, so the only record is the
organization index at _index._agents, aimed at the website over HTTPS.
A TXT record at _catalog._agents points at the ARD manifest.

DNSSEC signing starts in Cloudflare immediately. Validating resolvers
return authenticated data only after the registrar publishes the DS
record this script prints.
"""

import json
import os
import sys
import urllib.error
import urllib.parse
import urllib.request

TOKEN = os.environ.get("CLOUDFLARE_API_TOKEN", "")
ZONE_NAME = "tinyclient.io"
API = "https://api.cloudflare.com/client/v4"


def request(method, path, body=None):
    data = None if body is None else json.dumps(body).encode()
    req = urllib.request.Request(
        API + path,
        data=data,
        method=method,
        headers={
            "Authorization": "Bearer " + TOKEN,
            "Content-Type": "application/json",
        },
    )
    try:
        with urllib.request.urlopen(req) as resp:
            payload = json.load(resp)
    except urllib.error.HTTPError as err:
        detail = err.read().decode("utf-8", "replace")
        raise SystemExit(f"{method} {path} failed: {err.code} {detail}")
    if not payload.get("success", False):
        raise SystemExit(f"{method} {path} failed: {json.dumps(payload)}")
    return payload


def zone_id():
    query = urllib.parse.urlencode({"name": ZONE_NAME})
    payload = request("GET", "/zones?" + query)
    zones = payload.get("result") or []
    if not zones:
        raise SystemExit(f"no Cloudflare zone named {ZONE_NAME}")
    return zones[0]["id"]


def upsert(zid, record):
    name = record["name"]
    if not name.endswith("." + ZONE_NAME):
        fqdn = name + "." + ZONE_NAME
    else:
        fqdn = name
    query = urllib.parse.urlencode({"type": record["type"], "name": fqdn})
    found = request("GET", f"/zones/{zid}/dns_records?{query}").get("result") or []
    if found:
        existing = found[0]
        request("PUT", f"/zones/{zid}/dns_records/{existing['id']}", record)
        print(f"updated {record['type']} {fqdn}")
        return
    request("POST", f"/zones/{zid}/dns_records", record)
    print(f"created {record['type']} {fqdn}")


def dnssec(zid):
    current = request("GET", f"/zones/{zid}/dnssec")["result"]
    status = current.get("status")
    if status != "active":
        current = request("PATCH", f"/zones/{zid}/dnssec", {"status": "active"})["result"]
        print("DNSSEC status set to", current.get("status"))
    else:
        print("DNSSEC already active")
    print("Add this DS at the registrar if it is not there yet:")
    print(
        " ".join(
            str(current.get(key, ""))
            for key in ("key_tag", "algorithm", "digest_type", "digest")
        )
    )


def main():
    if not TOKEN:
        raise SystemExit("CLOUDFLARE_API_TOKEN is not set")
    zid = zone_id()
    upsert(
        zid,
        {
            "type": "HTTPS",
            "name": "_index._agents",
            "ttl": 3600,
            "proxied": False,
            "comment": "DNS-AID organization index. Target is the public website.",
            "data": {
                "priority": 1,
                "target": "tinyclient.io",
                "value": 'alpn="h2" port=443',
            },
        },
    )
    upsert(
        zid,
        {
            "type": "TXT",
            "name": "_catalog._agents",
            "ttl": 3600,
            "proxied": False,
            "content": "url=https://tinyclient.io/.well-known/ai-catalog.json",
            "comment": "ARD manifest location.",
        },
    )
    dnssec(zid)


if __name__ == "__main__":
    try:
        main()
    except SystemExit:
        raise
    except Exception as err:
        print(err, file=sys.stderr)
        raise SystemExit(1)
