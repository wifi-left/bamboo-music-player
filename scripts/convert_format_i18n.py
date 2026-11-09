"""Convert Chinese `format!` messages in lower modules to `crate::trf!`.

Only simple templates are converted:

* positional placeholders (`{}`), keeping the existing arguments;
* named captures (`{err}`), which are rewritten to positional and appended as
  explicit arguments.

Templates with format specs or a mix of named/positional placeholders are
reported and left for a manual edit. Test modules are never touched.
"""

from __future__ import annotations

import re
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
TARGETS = [
    ROOT / "src" / "api",
    ROOT / "src" / "audio",
    ROOT / "src" / "download",
    ROOT / "src" / "library",
    ROOT / "src" / "lyrics",
    ROOT / "src" / "util",
]
CJK = re.compile(r"[\u3400-\u4dbf\u4e00-\u9fff\uf900-\ufaff]")
NAME_PLACEHOLDER = re.compile(r"\{([a-zA-Z_][a-zA-Z0-9_]*)\}")


def find_calls(src: str, name: str):
    i = 0
    while True:
        i = src.find(name, i)
        if i < 0:
            return
        j = i + len(name)
        while j < len(src) and src[j].isspace():
            j += 1
        if j >= len(src) or src[j] != "(":
            i += 1
            continue
        depth = 0
        k = j
        in_str = False
        esc = False
        in_char = False
        in_line = False
        in_block = False
        while k < len(src):
            ch = src[k]
            if in_line:
                if ch == "\n":
                    in_line = False
                k += 1
                continue
            if in_block:
                if src.startswith("*/", k):
                    in_block = False
                    k += 2
                    continue
                k += 1
                continue
            if in_str:
                if esc:
                    esc = False
                elif ch == "\\":
                    esc = True
                elif ch == '"':
                    in_str = False
                k += 1
                continue
            if in_char:
                if esc:
                    esc = False
                elif ch == "\\":
                    esc = True
                elif ch == "'":
                    in_char = False
                k += 1
                continue
            if src.startswith("//", k):
                in_line = True
                k += 2
                continue
            if src.startswith("/*", k):
                in_block = True
                k += 2
                continue
            if ch == '"':
                in_str = True
                k += 1
                continue
            if ch == "'":
                # Char literal vs lifetime: a closing quote nearby means char.
                if (k + 2 < len(src) and src[k + 2] == "'") or (
                    k + 1 < len(src) and src[k + 1] == "\\"
                ):
                    in_char = True
                k += 1
                continue
            if ch == "(":
                depth += 1
            elif ch == ")":
                depth -= 1
                if depth == 0:
                    yield (i, j, k)
                    i = k + 1
                    break
            k += 1
        else:
            return


def split_args(src: str, start: int, end: int) -> list[str]:
    args: list[str] = []
    depth = 0
    last = start
    i = start
    in_str = False
    esc = False
    in_char = False
    while i < end:
        ch = src[i]
        if in_str:
            if esc:
                esc = False
            elif ch == "\\":
                esc = True
            elif ch == '"':
                in_str = False
        elif in_char:
            if esc:
                esc = False
            elif ch == "\\":
                esc = True
            elif ch == "'":
                in_char = False
        else:
            if ch == '"':
                in_str = True
            elif ch in "([{":
                depth += 1
            elif ch in ")]}":
                depth -= 1
            elif ch == "," and depth == 0:
                args.append(src[last:i])
                last = i + 1
        i += 1
    args.append(src[last:end])
    return args


def classify_template(template: str):
    """Return (kinds, names) or None when the template is not convertible."""
    placeholders: list[tuple[str, str | None]] = []
    i = 0
    while i < len(template):
        if template.startswith("{{", i) or template.startswith("}}", i):
            i += 2
            continue
        if template[i] == "{":
            end = template.find("}", i + 1)
            if end < 0:
                return None
            inner = template[i + 1 : end]
            if inner == "":
                placeholders.append(("pos", None))
            elif re.fullmatch(r"[a-zA-Z_][a-zA-Z0-9_]*", inner):
                placeholders.append(("name", inner))
            else:
                placeholders.append(("spec", inner))
            i = end + 1
            continue
        i += 1
    return placeholders


def main() -> None:
    changed = 0
    for base in TARGETS:
        for path in sorted(base.rglob("*.rs")):
            src = path.read_text(encoding="utf-8")
            marker = src.find("#[cfg(test)]")
            replacements = []
            skips = []
            for start, open_paren, close in find_calls(src, "format!"):
                if marker >= 0 and start >= marker:
                    continue
                args = split_args(src, open_paren + 1, close)
                first = args[0].strip()
                if len(first) < 2 or first[0] != '"' or first[-1] != '"':
                    continue
                if not CJK.search(first):
                    continue
                template = first[1:-1]
                placeholders = classify_template(template)
                if placeholders is None:
                    skips.append((first, "unbalanced"))
                    continue
                kinds = {kind for kind, _ in placeholders}
                if "spec" in kinds or "mixed" in kinds:
                    skips.append((first, "format spec"))
                    continue
                if kinds <= {"pos"}:
                    replacements.append(
                        (start, close + 1, "crate::trf!" + src[open_paren : close + 1])
                    )
                    continue
                if kinds == {"name"}:
                    new_template = NAME_PLACEHOLDER.sub("{}", template)
                    extra = [name for _, name in placeholders if name]
                    existing = [arg.strip() for arg in args[1:] if arg.strip()]
                    all_args = existing + extra
                    body = ", ".join(all_args)
                    replacements.append(
                        (
                            start,
                            close + 1,
                            f'crate::trf!("{new_template}"{", " + body if body else ""})',
                        )
                    )
                    continue
                skips.append((first, "mixed placeholders"))
            for first, reason in skips:
                print(f"SKIP {path.relative_to(ROOT)}: {reason}: {first}")
            if not replacements:
                continue
            for start, end, new in sorted(replacements, reverse=True):
                src = src[:start] + new + src[end:]
            path.write_text(src, encoding="utf-8")
            changed += len(replacements)
            print(f"{path.relative_to(ROOT)}: {len(replacements)}")
    print(f"converted {changed} format! calls")


if __name__ == "__main__":
    main()
