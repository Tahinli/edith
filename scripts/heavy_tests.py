#!/usr/bin/env python3
"""Print the nextest filterset that keeps the heavy tests out of the `fast` profile.

Usage:
    python3 scripts/heavy_tests.py <nextest.log> [threshold_seconds=10]

Reads PASS/FAIL result lines from a full nextest run log, dedupes them on
(binary id, test name), and prints one TOML literal string for
`default-filter` in the `[profile.fast]` section of .config/nextest.toml.

Rerun it after a full run when slow tests come or go.
"""
import re
import sys
from collections import defaultdict

RESULT = re.compile(
    r"^\s*(?:PASS|FAIL)\s+\[\s*([\d.]+)s\]\s+\(\s*\d+/\d+\)\s+(\S+)\s+(\S.*?)\s*$"
)
ANSI = re.compile(r"\x1b\[[0-9;]*m")

# Integration binaries of `engine` other than the merged `it` binary are heavy.
ENGINE_PKG = "engine"
# Binaries whose tests are grouped by the first `::` segment of the name.
MODULE_BINARIES = ("engine::it", "edith_mcp::it")
MODULE_SHARE = 0.6


def load(path):
    tests = {}
    with open(path, encoding="utf-8", errors="replace") as f:
        for line in f:
            m = RESULT.match(ANSI.sub("", line))
            if m:
                secs, binary, name = m.groups()
                tests.setdefault((binary, name), float(secs))
    return tests


def in_engine_rule(binary):
    """Term (a): integration test binaries of engine, except `it`."""
    pkg, sep, rest = binary.partition("::")
    return pkg == ENGINE_PKG and bool(sep) and rest != "it" and not rest.startswith("bin/")


def build_terms(tests, threshold):
    # Group module-binary tests by module (only names that have a `::`).
    groups = defaultdict(lambda: defaultdict(list))
    for (binary, name), secs in tests.items():
        if binary in MODULE_BINARIES and "::" in name:
            groups[binary][name.split("::", 1)[0]].append((name, secs))

    heavy_modules = set()  # (binary, module)
    for binary, mods in groups.items():
        for module, items in mods.items():
            heavy = sum(1 for _, s in items if s >= threshold)
            if heavy / len(items) >= MODULE_SHARE:
                heavy_modules.add((binary, module))

    a_terms = [f"(package({ENGINE_PKG}) and kind(test) and not binary(it))"]
    b_terms = []
    for binary, module in sorted(heavy_modules):
        b_terms.append(f"(binary_id({binary}) and test(/^{re.escape(module)}::/))")

    c_terms = []
    for (binary, name), secs in sorted(tests.items()):
        if secs < threshold:
            continue
        if in_engine_rule(binary):
            continue
        if (binary, name.split("::", 1)[0]) in heavy_modules and "::" in name:
            continue
        c_terms.append(f"(binary_id({binary}) and test(={name}))")

    return a_terms, b_terms, c_terms


def toml_literal(expr):
    if "'" in expr:
        return "'''" + expr + "'''"
    return "'" + expr + "'"


def main():
    if len(sys.argv) not in (2, 3):
        sys.exit("usage: heavy_tests.py <nextest.log> [threshold_seconds=10]")
    threshold = float(sys.argv[2]) if len(sys.argv) == 3 else 10.0
    tests = load(sys.argv[1])
    a_terms, b_terms, c_terms = build_terms(tests, threshold)
    expr = "not (" + " or ".join(a_terms + b_terms + c_terms) + ")"
    print(f"{len(tests)} distinct tests, {len(a_terms)} (a), {len(b_terms)} (b), "
          f"{len(c_terms)} (c)", file=sys.stderr)
    print(toml_literal(expr))


if __name__ == "__main__":
    main()
