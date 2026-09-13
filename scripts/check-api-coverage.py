#!/usr/bin/env python3
"""Every route in the server API, against the OpenAPI spec.

The SDKs are written from `web/app/public/openapi.yaml`, so a route that
never reaches the spec never reaches any of them. That is not a theory:
campaigns, subscribers and layouts were routed since v0.5 and absent from
the spec until 2026-09-14, and all eight SDKs were missing exactly those
three surfaces.

Exits non-zero when the two disagree in either direction. A route with no
operation is an undocumented endpoint; an operation with no route is a
promise the server does not keep.
"""

from __future__ import annotations

import pathlib
import re
import sys

ROUTER = pathlib.Path("crates/camelmailer-api/src/server_api.rs")
SPEC = pathlib.Path("web/app/public/openapi.yaml")
PREFIX = "/api/v2/server"
METHODS = ("get", "post", "patch", "put", "delete")


def routed() -> set[tuple[str, str]]:
    """(METHOD, path) for every `.route("…", get(...).post(...))` in the router.

    The handler list is read by balancing parentheses rather than by regex,
    because a route's handlers routinely span several lines.
    """
    src = ROUTER.read_text()
    found: set[tuple[str, str]] = set()
    for match in re.finditer(r'\.route\(\s*"([^"]+)"\s*,', src):
        path = match.group(1) or "/"
        i, depth, buf = match.end(), 1, []
        while i < len(src) and depth > 0:
            char = src[i]
            if char == "(":
                depth += 1
            elif char == ")":
                depth -= 1
                if depth == 0:
                    break
            buf.append(char)
            i += 1
        for method in re.findall(r"(?:^|[^a-z_])(" + "|".join(METHODS) + r")\s*\(", "".join(buf)):
            found.add((method.upper(), path))
    return found


def documented() -> set[tuple[str, str]]:
    """(METHOD, path) for every server operation in the spec.

    Parsed line by line rather than with a YAML library so the check has no
    dependencies: a path key is two spaces deep, a method key is four.
    """
    found: set[tuple[str, str]] = set()
    path: str | None = None
    for line in SPEC.read_text().splitlines():
        if line.startswith("  /") and line.rstrip().endswith(":"):
            key = line.strip().rstrip(":")
            path = key[len(PREFIX):] or "/" if key.startswith(PREFIX) else None
        elif path is not None and line.startswith("    ") and not line.startswith("     "):
            method = line.strip().rstrip(":").lower()
            if method in METHODS:
                found.add((method.upper(), path))
    return found


def main() -> int:
    impl, spec = routed(), documented()
    print(f"routes: {len(impl)}   documented: {len(spec)}")
    problems = 0
    for label, rows in (
        ("undocumented (missing from every SDK)", sorted(impl - spec, key=lambda r: r[1])),
        ("documented but not routed", sorted(spec - impl, key=lambda r: r[1])),
    ):
        if rows:
            problems += len(rows)
            print(f"\n{label} ({len(rows)}):", file=sys.stderr)
            for method, path in rows:
                print(f"  {method:<6} {path}", file=sys.stderr)
    if problems:
        print(f"\n{SPEC} and {ROUTER} disagree.", file=sys.stderr)
        return 1
    print("in step.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
