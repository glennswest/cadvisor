#!/usr/bin/env python3
"""Structural JSON API diff: real cadvisor (REF) vs cadvisor-rs (OURS).

For each endpoint, both responses are reduced to the set of key-paths with
JSON types (values ignored; container-id path segments and array indexes
collapsed), then compared. Exits nonzero on any unexpected delta.
"""
import json
import os
import re
import sys
import urllib.request

REF = os.environ.get("REF_BASE", "http://localhost:18080")
OURS = os.environ.get("OURS_BASE", "http://localhost:18081")

ENDPOINTS = [
    "/api/v1.3/machine",
    "/api/v1.3/containers/",
    "/api/v1.3/containers/system.slice",
    "/api/v1.3/subcontainers/machine.slice",
    "/api/v1.3/events/?all_events=true&max_events=10",
    "/api/v2.0/version",
    "/api/v2.0/attributes",
    "/api/v2.0/machine",
    "/api/v2.0/stats/machine.slice?count=3",
    "/api/v2.1/stats/?recursive=true&count=3",
    "/api/v2.1/machinestats",
    "/api/v2.0/spec/?recursive=true",
    "/api/v2.0/summary/machine.slice",
    "/api/v2.0/ps/",
    "/api/v2.0/storage",
]

# Path segments that are host/instance specific.
ID_SEGMENT = re.compile(r"^/(machine\.slice|system\.slice|user\.slice|init\.scope|dev-|sys-|proc-).*")


def fetch(base, path):
    with urllib.request.urlopen(base + path, timeout=30) as r:
        return json.loads(r.read())


def shape(obj, path, out):
    if isinstance(obj, dict):
        for k, v in obj.items():
            key = "<container>" if k.startswith("/") else k
            shape(v, f"{path}.{key}", out)
        if not obj:
            out.add(f"{path}:object")
    elif isinstance(obj, list):
        out.add(f"{path}:array")
        for v in obj:
            shape(v, f"{path}[]", out)
    elif isinstance(obj, bool):
        out.add(f"{path}:bool")
    elif isinstance(obj, (int, float)):
        out.add(f"{path}:number")
    elif obj is None:
        out.add(f"{path}:null")
    else:
        out.add(f"{path}:string")


def main():
    failures = 0
    for ep in ENDPOINTS:
        try:
            ref = fetch(REF, ep)
        except Exception as e:
            print(f"{ep}: SKIP (ref fetch failed: {e})")
            continue
        try:
            ours = fetch(OURS, ep)
        except Exception as e:
            print(f"{ep}: FAIL (ours fetch failed: {e})")
            failures += 1
            continue
        ref_shape, ours_shape = set(), set()
        shape(ref, "$", ref_shape)
        shape(ours, "$", ours_shape)
        missing = sorted(ref_shape - ours_shape)
        extra = sorted(ours_shape - ref_shape)
        if missing or extra:
            failures += 1
            print(f"{ep}: FAIL ({len(missing)} missing, {len(extra)} extra)")
            for m in missing[:15]:
                print(f"  missing: {m}")
            for e in extra[:15]:
                print(f"  extra:   {e}")
        else:
            print(f"{ep}: OK ({len(ref_shape)} paths)")
    sys.exit(1 if failures else 0)


if __name__ == "__main__":
    main()
