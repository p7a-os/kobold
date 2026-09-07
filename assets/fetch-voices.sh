#!/usr/bin/env bash
# Fill in the voice embeddings missing from the bundle, and refresh SHA256SUMS.
#
#   HF_TOKEN=hf_... ./fetch-voices.sh
#
# The bundle was assembled from a machine whose Hugging Face cache held only
# the voices that had actually been used. The rest need one authenticated
# fetch, because the upstream repository is gated -- that gate is exactly what
# publishing this mirror removes for end users, but whoever builds the mirror
# still has to pass through it once.
#
# Run this before upload.sh, or ship with fewer voices and keep kobold's VOICES
# list in step with what is actually served.
set -euo pipefail

REPO="kyutai/pocket-tts"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DEST="$HERE/pocket-tts/v1/embeddings"
# Must match kobold's VOICES in src/tts.rs. A voice listed there and missing
# here is a runtime 404 for whoever selects it.
VOICES=(alba marius javert jean fantine cosette eponine azelma)

: "${HF_TOKEN:?set HF_TOKEN -- accept the terms at https://huggingface.co/$REPO first}"
mkdir -p "$DEST"

got=0
for v in "${VOICES[@]}"; do
    out="$DEST/$v.safetensors"
    if [ -s "$out" ]; then
        echo "  have  $v"
        continue
    fi
    echo "  fetch $v"
    # --fail so a 401 or 404 does not land as a file full of HTML, which would
    # then checksum happily and fail much later as a corrupt tensor.
    if curl -fsSL --max-time 300 \
        -H "Authorization: Bearer $HF_TOKEN" \
        -o "$out.part" \
        "https://huggingface.co/$REPO/resolve/main/embeddings/$v.safetensors"; then
        mv "$out.part" "$out"
        got=$((got + 1))
    else
        rm -f "$out.part"
        echo "  FAILED $v -- gated repo not accepted, or the voice does not exist" >&2
    fi
done

echo "fetched $got new voice(s)"

# Sanity: a safetensors file starts with a little-endian u64 header length,
# and the header is JSON. An HTML error page fails this immediately.
for f in "$DEST"/*.safetensors; do
    python3 - "$f" <<'PY'
import json, struct, sys
p = sys.argv[1]
with open(p, 'rb') as f:
    n = struct.unpack('<Q', f.read(8))[0]
    if n > 100_000_000:
        sys.exit(f"{p}: implausible header length, not a safetensors file")
    json.loads(f.read(n))
PY
done
echo "all voice files parse as safetensors"

cd "$HERE/pocket-tts/v1"
sha256sum tokenizer.model tts_b6369a24.safetensors embeddings/*.safetensors NOTICE > SHA256SUMS
sha256sum -c SHA256SUMS >/dev/null
echo "SHA256SUMS refreshed and verified"
