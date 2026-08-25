# /// script
# requires-python = ">=3.11"
# dependencies = ["pyjwt[crypto]>=2.8", "requests>=2.31"]
# ///
"""TestFlight helper on the App Store Connect API (run with `uv run`).

Credentials come from the gitignored .env (ASC_KEY_ID, ASC_ISSUER_ID,
ASC_KEY_PATH); source it first: `set -a; . ./.env; set +a`.

  uv run scripts/testflight.py status
      Latest builds for the app and their processing state.
  uv run scripts/testflight.py distribute --group Internal [--build N]
                                          [--encryption exempt|non-exempt]
      Wait for the build to finish processing, record the export-compliance
      answer, and add it to the named beta group (created if missing, as an
      internal group so no Beta App Review is needed).
"""

import argparse
import os
import sys
import time

import jwt
import requests

API = "https://api.appstoreconnect.apple.com/v1"
BUNDLE_ID = "com.obsink.ios"


def token() -> str:
    key_id = os.environ["ASC_KEY_ID"]
    issuer = os.environ["ASC_ISSUER_ID"]
    with open(os.environ["ASC_KEY_PATH"]) as f:
        key = f.read()
    now = int(time.time())
    return jwt.encode(
        {"iss": issuer, "iat": now, "exp": now + 900, "aud": "appstoreconnect-v1"},
        key,
        algorithm="ES256",
        headers={"kid": key_id},
    )


def call(method: str, path: str, **kw):
    r = requests.request(method, f"{API}{path}", headers={"Authorization": f"Bearer {token()}"}, timeout=60, **kw)
    if r.status_code >= 400:
        sys.exit(f"{method} {path} -> {r.status_code}: {r.text[:800]}")
    return r.json() if r.text else {}


def app_id() -> str:
    apps = call("GET", "/apps", params={"filter[bundleId]": BUNDLE_ID})["data"]
    if not apps:
        sys.exit(f"no App Store Connect app record for {BUNDLE_ID} — create it in ASC first")
    return apps[0]["id"]


def builds(app: str, limit: int = 5):
    return call(
        "GET",
        "/builds",
        params={"filter[app]": app, "sort": "-uploadedDate", "limit": limit,
                "fields[builds]": "version,processingState,uploadedDate,usesNonExemptEncryption,expired"},
    )["data"]


def cmd_status(_args):
    app = app_id()
    for b in builds(app):
        a = b["attributes"]
        print(f"build {a['version']:>6}  {a['processingState']:<10}  uploaded {a['uploadedDate']}  "
              f"nonExemptEncryption={a.get('usesNonExemptEncryption')}  expired={a['expired']}")


def cmd_distribute(args):
    app = app_id()

    build = None
    deadline = time.time() + 40 * 60
    while time.time() < deadline:
        for b in builds(app, limit=10):
            if args.build and b["attributes"]["version"] != str(args.build):
                continue
            build = b
            break
        if build is None:
            print("build not visible yet; waiting…")
        else:
            state = build["attributes"]["processingState"]
            if state == "VALID":
                break
            if state in ("FAILED", "INVALID"):
                sys.exit(f"build {build['attributes']['version']} is {state}")
            print(f"build {build['attributes']['version']} is {state}; waiting…")
        time.sleep(30)
    else:
        sys.exit("timed out waiting for processing")

    bid = build["id"]
    if args.encryption:
        call("PATCH", f"/builds/{bid}", json={"data": {"type": "builds", "id": bid, "attributes": {
            "usesNonExemptEncryption": args.encryption == "non-exempt"}}})
        print(f"export compliance recorded: {args.encryption}")

    groups = call("GET", "/betaGroups", params={"filter[app]": app, "filter[name]": args.group})["data"]
    if groups:
        gid = groups[0]["id"]
        if groups[0]["attributes"].get("hasAccessToAllBuilds"):
            # Auto-access groups receive every build; assigning explicitly is
            # rejected (422 "Cannot add internal group to a build").
            print(f"build {build['attributes']['version']} is available to '{args.group}' (auto-access group)")
            return
    else:
        gid = call("POST", "/betaGroups", json={"data": {"type": "betaGroups", "attributes": {
            "name": args.group, "isInternalGroup": True, "hasAccessToAllBuilds": False},
            "relationships": {"app": {"data": {"type": "apps", "id": app}}}}})["data"]["id"]
        print(f"created internal beta group '{args.group}'")
    call("POST", f"/betaGroups/{gid}/relationships/builds", json={"data": [{"type": "builds", "id": bid}]})
    print(f"build {build['attributes']['version']} added to '{args.group}' — testers get the TestFlight push now")


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = p.add_subparsers(dest="cmd", required=True)
    sub.add_parser("status").set_defaults(fn=cmd_status)
    d = sub.add_parser("distribute")
    d.add_argument("--group", default="Internal")
    d.add_argument("--build", help="build number; default: newest")
    d.add_argument("--encryption", choices=["exempt", "non-exempt"])
    d.set_defaults(fn=cmd_distribute)
    args = p.parse_args()
    args.fn(args)


if __name__ == "__main__":
    main()
