"""Wrap Chinese UI string literals with crate::i18n::tr(...).

Run once from the repository root. The script deliberately skips format
strings (which contain braces) and lines that are part of formatting macros,
because Rust requires a literal there.
"""

from __future__ import annotations

import json
import re
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
TARGETS = [
    ROOT / "src" / "app",
    ROOT / "src" / "platform",
    ROOT / "src" / "api",
    ROOT / "src" / "audio",
    ROOT / "src" / "download",
    ROOT / "src" / "library",
    ROOT / "src" / "lyrics",
    ROOT / "src" / "util",
]
FORMAT_MARKERS = (
    "format!(",
    "write!(",
    "writeln!(",
    "println!(",
    "eprintln!(",
    "tracing::",
    "panic!(",
    "assert!(",
    "assert_eq!(",
    "assert_ne!(",
    "debug_assert",
)
CJK = re.compile(r"[\u3400-\u4dbf\u4e00-\u9fff\uf900-\ufaff]")


def skip_line(line: str) -> bool:
    code = line.split("//", 1)[0]
    return any(marker in code for marker in FORMAT_MARKERS)


def unescape(text: str) -> str:
    out = []
    i = 0
    while i < len(text):
        ch = text[i]
        if ch != "\\":
            out.append(ch)
            i += 1
            continue
        i += 1
        if i >= len(text):
            return text
        esc = text[i]
        i += 1
        mapping = {"n": "\n", "r": "\r", "t": "\t", "0": "\0", "\\": "\\", '"': '"', "'": "'"}
        if esc in mapping:
            out.append(mapping[esc])
        elif esc == "u" and i < len(text) and text[i] == "{":
            end = text.find("}", i)
            if end < 0:
                return text
            out.append(chr(int(text[i + 1 : end], 16)))
            i = end + 1
        else:
            # Unknown escape: keep the original spelling so we do not alter it.
            return text
    return "".join(out)


def scan(src: str):
    """Yield (start, end, content) for normal string literals."""
    spans = []
    i = 0
    n = len(src)
    line_comment = False
    block_comment = False
    while i < n:
        if line_comment:
            if src[i] == "\n":
                line_comment = False
            i += 1
            continue
        if block_comment:
            if src.startswith("*/", i):
                block_comment = False
                i += 2
            else:
                i += 1
            continue
        if src.startswith("//", i):
            line_comment = True
            i += 2
            continue
        if src.startswith("/*", i):
            block_comment = True
            i += 2
            continue
        ch = src[i]
        if ch == "'":
            # Character literal or lifetime. `'"'` must not start a string.
            if i + 2 < n and src[i + 2] == "'" and src[i + 1] != "\\":
                i += 3
                continue
            if i + 1 < n and src[i + 1] == "\\":
                j = i + 2
                while j < n:
                    if src[j] == "\\":
                        j += 2
                        continue
                    if src[j] == "'":
                        j += 1
                        break
                    if src[j] == "\n":
                        break
                    j += 1
                i = j
                continue
            i += 1
            continue
        if ch == '"':
            if i > 0 and src[i - 1] in "br":
                # Byte/raw string: leave these alone.
                i += 1
                continue
            start = i
            i += 1
            while i < n:
                if src[i] == "\\":
                    i += 2
                    continue
                if src[i] == '"':
                    i += 1
                    break
                i += 1
            spans.append((start, i, src[start + 1 : i - 1]))
            continue
        i += 1
    return spans


TR_CALL = re.compile(r'crate::i18n::tr\(\s*("(?:[^"\\]|\\.)*")')
TRF_CALL = re.compile(r'crate::trf!\(\s*("(?:[^"\\]|\\.)*")', re.DOTALL)


def main() -> None:
    changed = 0
    for base in TARGETS:
        for path in sorted(base.rglob("*.rs")):
            src = path.read_text(encoding="utf-8")
            test_marker = src.find("#[cfg(test)]")
            lines = src.splitlines(keepends=True)
            line_starts = []
            offset = 0
            for line in lines:
                line_starts.append(offset)
                offset += len(line)

            def line_of(position: int) -> str:
                lo, hi = 0, len(line_starts) - 1
                while lo < hi:
                    mid = (lo + hi + 1) // 2
                    if line_starts[mid] <= position:
                        lo = mid
                    else:
                        hi = mid - 1
                return lines[lo]

            replacements = []
            for start, end, raw in scan(src):
                if test_marker >= 0 and start >= test_marker:
                    continue
                if "{" in raw or "}" in raw:
                    continue
                if not CJK.search(raw):
                    continue
                if re.search(
                    r"(?:tr|trf)\(\s*$", src[max(0, start - 300) : start]
                ):
                    continue
                if skip_line(line_of(start)):
                    continue
                value = unescape(raw)
                if value == raw and "\\" in raw:
                    continue
                # Language-independent logic and persisted track data.
                if value == "等线":
                    continue
                # Region aliases are match patterns, not UI text.
                if value in ("中文", "日本語"):
                    continue
                if value in ("本地", "在线") and "source_name:" in line_of(start):
                    continue
                replacements.append((start, end, f"crate::i18n::tr({src[start:end]})"))
            if not replacements:
                continue
            for start, end, replacement in reversed(replacements):
                src = src[:start] + replacement + src[end:]
            path.write_text(src, encoding="utf-8")
            changed += len(replacements)
            print(f"{path.relative_to(ROOT)}: {len(replacements)}")

    # Rebuild the coverage list from every tr("...") call, including ones
    # wrapped by earlier runs. Test modules are excluded.
    strings: set[str] = set()
    for base in TARGETS:
        for path in sorted(base.rglob("*.rs")):
            text = path.read_text(encoding="utf-8")
            marker = text.find("#[cfg(test)]")
            if marker >= 0:
                text = text[:marker]
            # Static labels (tr) and formatted templates (trf!) both need an
            # English entry; the runtime coverage test reads this list.
            for pattern in (TR_CALL, TRF_CALL):
                for match in pattern.finditer(text):
                    value = unescape(match.group(1)[1:-1])
                    if value != match.group(1)[1:-1] or "\\" not in match.group(1):
                        strings.add(value)

    out = ROOT / "scripts" / "i18n_strings.json"
    out.write_text(
        json.dumps(sorted(strings), ensure_ascii=False, indent=1) + "\n", encoding="utf-8"
    )
    print(f"wrapped {changed} literals, {len(strings)} unique -> {out}")


if __name__ == "__main__":
    main()
