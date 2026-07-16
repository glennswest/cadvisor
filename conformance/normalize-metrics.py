#!/usr/bin/env python3
"""Normalizes a Prometheus text exposition for structural diffing.

Emits sorted lines:
  HELP <family> <help text>
  TYPE <family> <type>
  SERIES <family>{<sorted label names, device/interface values masked>}

Values and timestamps are dropped (they legitimately differ between two
scrapers); go_*/process_* runtime families are skipped (allowlisted delta:
cadvisor-rs is not a Go program).
"""
import re
import sys

SKIP_PREFIXES = ("go_", "process_")
# Label values that depend on which mount namespace the scraper runs in.
MASKED_LABEL_VALUES = ("device",)
# Families whose label values are self-describing (version, build revision,
# scraper's own OS image) and legitimately differ between implementations.
MASK_ALL_VALUES_FAMILIES = ("cadvisor_version_info",)


def normalize(text: str):
    out = set()
    for line in text.splitlines():
        if line.startswith("# HELP ") or line.startswith("# TYPE "):
            kind, rest = line[2:].split(" ", 1)
            name = rest.split(" ", 1)[0]
            if name.startswith(SKIP_PREFIXES):
                continue
            out.add(f"{kind} {rest}")
        elif line and not line.startswith("#"):
            m = re.match(r"^([a-zA-Z_:][a-zA-Z0-9_:]*)(\{(.*)\})?\s", line + " ")
            if not m:
                continue
            name = m.group(1)
            if name.startswith(SKIP_PREFIXES):
                continue
            labels = []
            body = m.group(3) or ""
            for lm in re.finditer(r'([a-zA-Z0-9_]+)="((?:[^"\\]|\\.)*)"', body):
                k, v = lm.group(1), lm.group(2)
                if k in MASKED_LABEL_VALUES or name in MASK_ALL_VALUES_FAMILIES:
                    v = "<masked>"
                labels.append(f'{k}="{v}"')
            out.add(f"SERIES {name}{{{','.join(sorted(labels))}}}")
    return sorted(out)


if __name__ == "__main__":
    text = open(sys.argv[1]).read() if len(sys.argv) > 1 else sys.stdin.read()
    print("\n".join(normalize(text)))
