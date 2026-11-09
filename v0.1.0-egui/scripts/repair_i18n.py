"""Repair the second wrapping pass: undo double wraps and test-only wraps,
then rebuild i18n_strings.json from every remaining tr("...") call."""

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
STRING = r'"(?:[^"\\]|\\.)*"'
DOUBLE = re.compile(rf"crate::i18n::tr\(\s*(crate::i18n::tr\(\s*{STRING}\s*\))\s*,?\s*\)")
SINGLE = re.compile(rf"crate::i18n::tr\(\s*({STRING})\s*,?\s*\)")


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
            return text
    return "".join(out)


def main() -> None:
    for base in TARGETS:
        for path in sorted(base.rglob("*.rs")):
            text = path.read_text(encoding="utf-8")
            previous = None
            while previous != text:
                previous = text
                text = DOUBLE.sub(r"\1", text)

            # Unwrap tr(...) inside test modules.
            marker = re.search(r"#\[cfg\(test\)\]", text)
            split = marker.start() if marker else len(text)
            head, tail = text[:split], text[split:]
            tail = SINGLE.sub(r"\1", tail)
            text = head + tail

            # Canonical data used for language-independent logic / persistence.
            text = text.replace('crate::i18n::tr("等线")', '"等线"')
            text = text.replace(
                'source_name: crate::i18n::tr("在线").to_string()',
                'source_name: "在线".to_string()',
            )
            text = text.replace(
                'source_name: crate::i18n::tr("本地").to_string()',
                'source_name: "本地".to_string()',
            )
            path.write_text(text, encoding="utf-8")

    strings: set[str] = set()
    for base in TARGETS:
        for path in sorted(base.rglob("*.rs")):
            text = path.read_text(encoding="utf-8")
            marker = re.search(r"#\[cfg\(test\)\]", text)
            if marker:
                text = text[: marker.start()]
            for match in SINGLE.finditer(text):
                value = unescape(match.group(1)[1:-1])
                strings.add(value)

    out = ROOT / "scripts" / "i18n_strings.json"
    out.write_text(
        json.dumps(sorted(strings), ensure_ascii=False, indent=1) + "\n", encoding="utf-8"
    )
    en = json.load(open(ROOT / "src/i18n/en.json", encoding="utf-8"))
    missing = [text for text in sorted(strings) if text not in en]
    print(f"{len(strings)} strings, {len(missing)} missing translations")
    for text in missing:
        print(repr(text))


if __name__ == "__main__":
    main()
