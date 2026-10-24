#!/usr/bin/env python3
"""Fail when authored Rust has code lines rustfmt could not wrap.

rustfmt leaves a whole statement unformatted when one of its pieces cannot fit,
and still reports success. A line whose code, with string literal contents
elided, is wider than `max_width` therefore marks a statement rustfmt silently
skipped. A literal that cannot be broken (a raw string or URL) may overflow on
its own.
Usage: scripts/check-rust-width.py [FILE...]   (default: tracked authored Rust)
"""
import re
import subprocess
import sys

MAX_WIDTH = 100


def string_line_spans(source):
    """Line numbers (1-based) whose content is entirely inside a multi-line string literal."""
    inside, i, line = set(), 0, 1
    while i < len(source):
        c = source[i]
        if source.startswith("//", i):
            j = source.find("\n", i)
            i = len(source) if j < 0 else j
            continue
        if source.startswith("/*", i):
            j = source.find("*/", i) + 2
            line += source.count("\n", i, j)
            i = j
            continue
        raw = re.match(r'b?r(#*)"', source[i:i + 6]) if c in "br" and (i == 0 or not (source[i - 1].isalnum() or source[i - 1] == "_")) else None
        if raw or c == '"':
            if raw:
                end = source.index('"' + raw.group(1), i + len(raw.group(0))) + 1 + len(raw.group(1))
            else:
                end = i + 1
                while source[end] != '"':
                    end += 2 if source[end] == "\\" else 1
                end += 1
            lines = source.count("\n", i, end)
            inside.update(range(line + 1, line + lines + 1))
            line += lines
            i = end
            continue
        if c == "'" and i + 2 < len(source) and (source[i + 2] == "'" or (source[i + 1] == "\\" and source[i + 3:i + 4] == "'")):
            i += 3 if source[i + 1] != "\\" else 4
            continue
        if c == "\n":
            line += 1
        i += 1
    return inside


def elide_literals(line):
    """The line with each complete string literal's contents removed."""
    line = re.sub(r'b?r(#+)"(.*?)"\1', lambda m: 'r' + m.group(1) + '""' + m.group(1), line)
    line = re.sub(r'"(?:[^"\\]|\\.)*"', '""', line)
    # A literal that continues on the next line elides to its opening quote.
    return re.sub(r'"(?:[^"\\]|\\.)*\\?$', '"', line)


def main():
    paths = sys.argv[1:] or [
        path
        for path in subprocess.check_output(["git", "ls-files", "*.rs"], text=True).split()
        if "/generated/" not in path
    ]
    failures = 0
    for path in paths:
        source = open(path, encoding="utf-8").read()
        allowed = string_line_spans(source)
        # rustfmt fully formats `use` declarations; any overflow left there is
        # an identifier too long to split, not a skipped statement.
        in_use = False
        for number, text in enumerate(source.split("\n"), 1):
            stripped = text.strip()
            if re.match(r"(pub(\([^)]*\))? )?use ", stripped):
                in_use = True
            if in_use:
                in_use = not stripped.endswith(";")
                continue
            if len(text) > MAX_WIDTH and number not in allowed and len(elide_literals(text)) > MAX_WIDTH:
                failures += 1
                print(f"{path}:{number}: {len(text)} columns")
    if failures:
        print(f"{failures} Rust lines exceed {MAX_WIDTH} columns of code;"
              " rustfmt skipped their statements. Split long literals or restructure.", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
