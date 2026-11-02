#!/usr/bin/env python3
"""Fails unless the wasm's `__wbindgen_externrefs` export is an externref table.

wasm-bindgen's JS grows that table at start-up. binaryen 108's wasm-opt (Ubuntu
24.04's package) re-pointed the export at the funcref table, whose max == min,
so the page died with "RangeError: failed to grow table" (2026-11-02). Run by
scripts/web_release.sh after wasm-opt.
"""
import sys

FUNCREF, EXTERNREF = 0x70, 0x6F


def leb(b, i):
    r = s = 0
    while True:
        x = b[i]
        i += 1
        r |= (x & 0x7F) << s
        s += 7
        if x < 0x80:
            return r, i


def main(path):
    b = open(path, "rb").read()
    tables, exported, i = [], None, 8
    while i < len(b):
        sid = b[i]
        i += 1
        n, i = leb(b, i)
        end = i + n
        if sid == 2:  # imports: imported tables come first in the index space
            c, j = leb(b, i)
            for _ in range(c):
                for _ in range(2):
                    l, j = leb(b, j)
                    j += l
                kind = b[j]
                j += 1
                if kind == 0:
                    _, j = leb(b, j)
                elif kind == 1:
                    tables.append(b[j])
                    j += 1
                    fl = b[j]
                    j += 1
                    _, j = leb(b, j)
                    if fl & 1:
                        _, j = leb(b, j)
                elif kind == 2:
                    fl = b[j]
                    j += 1
                    _, j = leb(b, j)
                    if fl & 1:
                        _, j = leb(b, j)
                elif kind == 3:
                    j += 2
                else:
                    sys.exit(f"{path}: unknown import kind {kind}")
        elif sid == 4:
            c, j = leb(b, i)
            for _ in range(c):
                tables.append(b[j])
                j += 1
                fl = b[j]
                j += 1
                _, j = leb(b, j)
                if fl & 1:
                    _, j = leb(b, j)
        elif sid == 7:
            c, j = leb(b, i)
            for _ in range(c):
                l, j = leb(b, j)
                name = b[j : j + l].decode()
                j += l
                kind = b[j]
                j += 1
                idx, j = leb(b, j)
                if kind == 1 and name == "__wbindgen_externrefs":
                    exported = idx
        i = end
    if exported is None:
        return  # no externref table (reference-types off): nothing to check
    if exported >= len(tables) or tables[exported] != EXTERNREF:
        sys.exit(
            f"{path}: __wbindgen_externrefs exports table {exported} "
            f"({'funcref' if exported < len(tables) and tables[exported] == FUNCREF else 'not externref'}) "
            "— wasm-opt broke the wasm-bindgen output (too old a binaryen?)"
        )


if __name__ == "__main__":
    main(sys.argv[1])
