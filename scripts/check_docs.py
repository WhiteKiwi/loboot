#!/usr/bin/env python3
"""Offline documentation checks. Never execute commands copied from Markdown."""
from pathlib import Path
import re
import sys

ROOT = Path(__file__).resolve().parents[1]

def check():
    errors = []
    documents = sorted(ROOT.glob("*.md")) + sorted((ROOT / "docs").glob("*.md"))
    for document in documents:
        text = document.read_text()
        # Ignore fenced examples, where parentheses are not necessarily links.
        prose = re.sub(r"```.*?```", "", text, flags=re.S)
        for destination in re.findall(r"\]\(([^)\s]+)(?:\s+[^)]*)?\)", prose):
            if "://" in destination or destination.startswith(("#", "mailto:")):
                continue
            target = destination.split("#", 1)[0]
            if target and not (document.parent / target).exists():
                errors.append(f"{document.relative_to(ROOT)}: missing link {destination}")
    source = (ROOT / "src/main.rs").read_text().split("enum Commands {", 1)[1].split("\n}", 1)[0]
    commands = [word.lower() for word in re.findall(r"^    ([A-Z]\w+)(?:\s*\{|,)", source, re.M)]
    cli = (ROOT / "docs/CLI.md").read_text().lower()
    for command in commands:
        if not re.search(rf"\b{command}\b", cli):
            errors.append(f"docs/CLI.md: missing command {command}")
    if errors:
        print("\n".join(errors), file=sys.stderr)
        return 1
    print(f"Checked {len(documents)} Markdown files and {len(commands)} CLI commands (offline).")
    return 0

if __name__ == "__main__":
    raise SystemExit(check())
