#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
KNOWLEDGE_DIR="$REPO_ROOT/.knowledge"

echo "Checking knowledge base directory: $KNOWLEDGE_DIR"

REQUIRED_FILES=("dictionary.md" "facts.md" "intents.md" "decisions.md")
for f in "${REQUIRED_FILES[@]}"; do
    FILE_PATH="$KNOWLEDGE_DIR/$f"
    if [[ ! -f "$FILE_PATH" ]]; then
        echo "FAIL: Missing $f" >&2
        exit 1
    fi
    if ! grep -q "^## Current" "$FILE_PATH"; then
        echo "FAIL: $f missing '## Current' section" >&2
        exit 1
    fi
    if ! grep -q "^## History" "$FILE_PATH"; then
        echo "FAIL: $f missing '## History' section" >&2
        exit 1
    fi
    echo "OK: $f has required sections"
done

# Check Dictionary IDs (T-<slug>)
python3 -c '
import sys, re
from pathlib import Path

repo_root = Path("'"$REPO_ROOT"'")
kb = repo_root / ".knowledge"

def check_file(path, id_pattern, required_fields):
    content = path.read_text(encoding="utf-8")
    entries = re.findall(r"^### (.*)$", content, re.MULTILINE)
    ids_found = set()
    for e in entries:
        if not re.match(id_pattern, e):
            print(f"FAIL in {path.name}: header {e} does not match {id_pattern}", file=sys.stderr)
            sys.exit(1)
        if e in ids_found:
            print(f"FAIL in {path.name}: duplicate ID {e}", file=sys.stderr)
            sys.exit(1)
        ids_found.add(e)
    
    # Check that required fields exist per entry block
    blocks = re.split(r"^### ", content, flags=re.MULTILINE)[1:]
    for block in blocks:
        for rf in required_fields:
            if not re.search(rf, block, re.MULTILINE):
                header = block.splitlines()[0]
                print(f"FAIL in {path.name} ({header}): missing field {rf}", file=sys.stderr)
                sys.exit(1)

check_file(kb / "dictionary.md", r"^T-[a-z0-9-]+$", [r"\*\*ID\*\*:", r"\*\*Term\*\*:", r"\*\*Definition\*\*:", r"\*\*Status\*\*:", r"\*\*Source\*\*:", r"\*\*Date\*\*:"])
check_file(kb / "facts.md", r"^F-\d+$", [r"\*\*ID\*\*:", r"\*\*Statement\*\*:", r"\*\*Status\*\*:", r"\*\*Valid\*\*:", r"\*\*Source\*\*:", r"\*\*Date\*\*:"])
check_file(kb / "intents.md", r"^I-\d+$", [r"\*\*ID\*\*:", r"\*\*Statement\*\*:", r"\*\*Status\*\*:", r"\*\*Valid\*\*:", r"\*\*Source\*\*:", r"\*\*Date\*\*:"])
check_file(kb / "decisions.md", r"^D-\d+$", [r"\*\*ID\*\*:", r"\*\*Statement\*\*:", r"\*\*Status\*\*:", r"\*\*Valid\*\*:", r"\*\*Source\*\*:", r"\*\*Date\*\*:"])

print("OK: All knowledge base entries strictly conform to schema.")
'

echo "All verification checks passed."
