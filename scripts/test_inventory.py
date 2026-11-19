#!/usr/bin/env python3
"""Every test in the repository, and the rule that keeps only regression tests and gates.

    test_inventory.py inventory          > TSV of every test: where it is, the commit that added it, its marker
    test_inventory.py check [paths...]   exit 1 listing every test whose doc comment has no `Regression:` /
                                         `Gate:` line (Rust `///`, node `//`, python `#` right above the test;
                                         vendored forks excluded)

Plan: docs/superpowers/plans/2026-11-19-regression-only-tests.md. The rule (CLAUDE.md, Testing): a test is written
only as a regression for a bug seen in play or in a log.
"""
import os
import re
import subprocess
import sys
from collections import defaultdict

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
RUST_DIRS = ["crates", "tools", "prototypes"]
VENDORED = ("crates/rapier/", "crates/parry/", "crates/slotmapd/")
TEST_ATTR = re.compile(r"^\s*#\[(tokio::test|test|wasm_bindgen_test(::wasm_bindgen_test)?)\b")
FN = re.compile(r"^\s*(pub(\([^)]*\))?\s+)?(async\s+)?fn\s+(\w+)")
MARKER = re.compile(
    r"(?i)regression|was a bug|used to (panic|crash|fail|hang)|desync|\blive\b,|found (in|by)|repro"
)
RULE_MARK = re.compile(r"^\s*///\s*(Regression|Gate):")
# D2: the gates, by file (and test name prefix where the file holds more than the gate).
GATES = [
    ("crates/game/tests/wasm_determinism.rs", ""),
    ("crates/game/tests/physics_goldens.rs", ""),
    ("crates/physics-next/tests/state_hash_wasm.rs", ""),
    ("crates/worldgen/tests/scene_hash_golden.rs", ""),
    ("crates/worldgen/tests/forge_lib.rs", "forge_quality_matches_baseline"),
    ("crates/worldgen/src/pipeline.rs", "fused_column_matches_density_graphs_bit_exact"),
    ("crates/worldgen/src/pipeline.rs", "column_profile_matches_graph_bit_exact"),
]


def rel(p):
    return os.path.relpath(p, ROOT)


def crate_of(path):
    parts = path.split("/")
    return "/".join(parts[:2]) if parts[0] in ("crates", "tools", "prototypes") else parts[0]


def rust_files(paths):
    for base in paths:
        full = os.path.join(ROOT, base)
        if os.path.isfile(full):
            yield rel(full)
            continue
        for d, dirs, files in os.walk(full):
            dirs[:] = [x for x in dirs if x not in ("target", ".git", "node_modules")]
            for f in files:
                if f.endswith(".rs"):
                    yield rel(os.path.join(d, f))


def find_tests(path):
    """(line, name, doc_lines, body_lines) for each test fn in a Rust file."""
    with open(os.path.join(ROOT, path), encoding="utf-8", errors="replace") as f:
        return find_tests_text(f.read().split("\n"))


def find_tests_text(lines):
    """find_tests over a file's lines."""
    out = []
    i = 0
    while i < len(lines):
        if TEST_ATTR.match(lines[i]):
            j = i
            name = None
            while j < min(i + 8, len(lines)):
                m = FN.match(lines[j])
                if m:
                    name = m.group(4)
                    break
                j += 1
            if name is None:
                i += 1
                continue
            # The comment/attribute block above the fn.
            k = j - 1
            while k >= 0 and re.match(r"^\s*(//|#\[|#!\[)", lines[k]):
                k -= 1
            doc = lines[k + 1 : j]
            indent = len(lines[j]) - len(lines[j].lstrip())
            end = j + 1
            while end < len(lines):
                ln = lines[end]
                if ln.strip() == "}" and len(ln) - len(ln.lstrip()) <= indent:
                    break
                end += 1
            out.append((j + 1, name, doc, lines[j : end + 1]))
            i = end + 1
        else:
            i += 1
    return out


def fn_origins():
    """name -> [(commit_index, short, subject, path)] for every commit that added a line `fn name(`."""
    cmd = [
        "git", "log", "--reverse", "--no-renames", "--format=@@COMMIT %h %s", "-p", "--unified=0", "--",
        "*.rs", ":(exclude)crates/rapier", ":(exclude)crates/parry", ":(exclude)crates/slotmapd", ":(exclude)quest",
    ]
    proc = subprocess.Popen(cmd, cwd=ROOT, stdout=subprocess.PIPE, text=True, errors="replace")
    origins = defaultdict(list)
    idx = -1
    short = subject = path = ""
    add_fn = re.compile(r"^\+\s*(pub(\([^)]*\))?\s+)?(async\s+)?fn\s+(\w+)\s*[(<]")
    for line in proc.stdout:
        if line.startswith("@@COMMIT "):
            idx += 1
            short, _, subject = line[9:].rstrip("\n").partition(" ")
        elif line.startswith("+++ "):
            path = line[6:].rstrip("\n") if line.startswith("+++ b/") else ""
        elif line.startswith("+") and "fn " in line:
            m = add_fn.match(line)
            if m:
                origins[m.group(4)].append((idx, short, subject, path))
    proc.wait()
    return origins


def origin_for(origins, name, path):
    cands = origins.get(name, [])
    crate = crate_of(path)
    pref = [c for c in cands if c[3] == path or crate_of(c[3]) == crate]
    pick = (pref or cands)
    if not pick:
        return "", ""
    first = min(pick, key=lambda c: c[0])
    return first[1], first[2]


def is_gate(path, name):
    return any(path == file and (not test or name == test) for file, test in GATES)


def propose(path, name, marker, subject):
    if path.startswith(VENDORED):
        return "vendored", "vendored fork (D3)"
    if is_gate(path, name):
        return "keep-gate", "gate (D2)"
    fix = subject.lower().startswith("fix")
    if path.startswith("crates/physics-next/"):
        return "review", "physics-next: fork pin? (D3)"
    if marker and fix:
        return "keep-regression", "marker + fix commit"
    if marker:
        return "review", "marker, origin not a fix"
    if fix:
        return "review", "fix commit, no marker"
    return "delete", "no marker, origin not a fix"


def clean(s):
    return s.replace("\t", " ").strip()[:120]


def dirty_paths():
    """Paths with uncommitted changes (untracked dirs expanded): someone's work in progress, never edited blind."""
    out = subprocess.run(["git", "status", "--porcelain", "--untracked-files=all"], cwd=ROOT,
                         capture_output=True, text=True).stdout
    return {ln[3:].strip('"') for ln in out.splitlines()}


def inventory():
    origins = fn_origins()
    dirty = dirty_paths()
    print("\t".join(["crate", "file", "line", "name", "kind", "origin", "origin_subject", "marker", "proposed", "reason", "bug"]))
    for path in sorted(rust_files(RUST_DIRS)):
        for line, name, doc, body in find_tests(path):
            if any("wasm_bindgen_test" in d for d in doc):
                kind = "wasm"
            else:
                kind = "integration" if "/tests/" in path else "unit"
            marker = ""
            for ln in doc + body:
                if ln.strip().startswith("//") and MARKER.search(ln):
                    marker = clean(ln.strip().lstrip("/").strip())
                    break
            short, subject = origin_for(origins, name, path)
            prop, reason = propose(path, name, marker, subject)
            if path in dirty and prop in ("delete", "keep-regression"):
                prop, reason = "review", reason + "; file has uncommitted changes"
            print("\t".join([crate_of(path), path, str(line), name, kind, short, clean(subject), marker, prop, reason, ""]))
    for path in sorted(_glob("website/tests", ".test.mjs")):
        with open(os.path.join(ROOT, path)) as f:
            for n, ln in enumerate(f, 1):
                m = re.match(r"^\s*(test|it)\(\s*['\"`](.+?)['\"`]", ln)
                if m:
                    print("\t".join(["website", path, str(n), clean(m.group(2)), "js", "", "", "", "delete", "no marker, origin not a fix", ""]))
    for path in sorted(_glob("scripts/tests", ".py")):
        with open(os.path.join(ROOT, path)) as f:
            for n, ln in enumerate(f, 1):
                m = re.match(r"^\s*def (test_\w+)", ln)
                if m:
                    print("\t".join(["scripts", path, str(n), m.group(1), "py", "", "", "", "delete", "no marker, origin not a fix", ""]))


def _glob(d, suffix):
    full = os.path.join(ROOT, d)
    if not os.path.isdir(full):
        return []
    return [os.path.join(d, f) for f in os.listdir(full) if f.endswith(suffix)]


OTHER_TESTS = [
    # (dir, file suffix, test line, comment prefix) — the marker sits in the comment lines right above the test.
    ("website/tests", ".test.mjs", re.compile(r"^\s*(test|it)\(\s*['\"`](.+?)['\"`]"), "//"),
    ("scripts/tests", ".py", re.compile(r"^\s*def (test_\w+)"), "#"),
]


def other_tests(paths):
    """(path, line, name, marker kind or None) for the website's node tests and the scripts' unittest tests."""
    for d, suffix, test_re, prefix in OTHER_TESTS:
        if paths and not any(d.startswith(p.rstrip("/")) or p.startswith(d) for p in paths):
            continue
        for path in sorted(_glob(d, suffix)):
            lines = open(os.path.join(ROOT, path)).read().split("\n")
            for i, ln in enumerate(lines):
                m = test_re.match(ln)
                if not m:
                    continue
                kind = None
                k = i - 1
                while k >= 0 and lines[k].strip().startswith((prefix, "@")):
                    mk = re.match(r"^\s*" + re.escape(prefix) + r"\s*(Regression|Gate):", lines[k])
                    if mk:
                        kind = mk.group(1)
                    k -= 1
                yield path, i + 1, m.group(m.lastindex), kind


def check(paths):
    bad = []
    counts = {"Regression": 0, "Gate": 0}
    for path, line, name, kind in other_tests(paths):
        if kind:
            counts[kind] += 1
        else:
            bad.append(f"{path}:{line} {name}")
    rust_paths = [p for p in paths if not p.startswith(("website", "scripts"))] if paths else RUST_DIRS
    for path in sorted(rust_files(rust_paths) if rust_paths else []):
        if path.startswith(VENDORED):
            continue
        for line, name, doc, _ in find_tests(path):
            kinds = [RULE_MARK.match(d).group(1) for d in doc if RULE_MARK.match(d)]
            if kinds:
                counts[kinds[0]] += 1
            else:
                bad.append(f"{path}:{line} {name}")
    if bad:
        print("\n".join(bad))
        print(f"test-audit: {len(bad)} test(s) without a `/// Regression:` or `/// Gate:` line "
              "(CLAUDE.md, Testing: a test is written only as a regression for a bug seen in play or in a log)", file=sys.stderr)
        return 1
    print(f"test-audit: {counts['Regression']} regression, {counts['Gate']} gate")
    return 0


if __name__ == "__main__":
    if len(sys.argv) < 2 or sys.argv[1] not in ("inventory", "check"):
        print(__doc__, file=sys.stderr)
        sys.exit(2)
    if sys.argv[1] == "inventory":
        inventory()
    else:
        sys.exit(check(sys.argv[2:]))
