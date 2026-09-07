#!/usr/bin/env bash
# Publish the model bundle to S3.
#
#   ./upload.sh my-bucket [prefix]
#
# The layout under the prefix mirrors the paths the engine already asks for,
# so switching kobold-tts from Hugging Face to this mirror is a base URL and
# nothing else.
#
# Objects go up immutable: the prefix carries a version, so a new set of
# weights becomes v2 rather than overwriting v1 under clients that have
# already cached it. That is what lets Cache-Control be a year.
set -euo pipefail

VERIFY=0
if [ "${1:-}" = "--verify" ]; then
    VERIFY=1
    shift
fi

BUCKET="${1:?usage: upload.sh [--verify] <bucket> [prefix]}"
PREFIX="${2:-pocket-tts/v1}"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SRC="$HERE/pocket-tts/v1"

# A bucket whose name contains a dot cannot be reached over HTTPS in
# virtual-hosted style: the S3 certificate is wildcarded one label deep, so
# made. Such buckets have to be addressed path-style, where the
# virtual-hosted forms fail TLS and path-style answers cleanly.
#
# Override for CloudFront or a custom domain, which is the nicer end state and
# sidesteps this entirely.
REGION="${AWS_REGION:-$(aws s3api get-bucket-location --bucket "$BUCKET" \
    --query LocationConstraint --output text 2>/dev/null)}"
[ "$REGION" = "None" ] || [ -z "$REGION" ] && REGION="us-east-1"
case "$BUCKET" in
    *.*) DEFAULT_BASE="https://s3.$REGION.amazonaws.com/$BUCKET/$PREFIX" ;;
    *)   DEFAULT_BASE="https://$BUCKET.s3.$REGION.amazonaws.com/$PREFIX" ;;
esac
BASE="${KOBOLD_ASSET_BASE:-$DEFAULT_BASE}"

# Fetch as a stranger would: no credentials, no signature. An upload that
# succeeded while the objects stayed private looks fine from here and fails on
# every user's machine, so this is the check that actually matters.
if [ "$VERIFY" = 1 ]; then
    echo "fetching $BASE/SHA256SUMS anonymously"
    sums=$(curl -fsSL --max-time 30 "$BASE/SHA256SUMS") || {
        echo "FAILED: cannot read SHA256SUMS without credentials." >&2
        echo "The bucket policy below has not been applied, or the prefix is wrong." >&2
        exit 1
    }
    fail=0
    while read -r _ name; do
        [ -n "$name" ] || continue
        hdrs=$(curl -sS -D - -o /dev/null -I --max-time 30 "$BASE/$name" 2>/dev/null | tr -d '\r' || true)
        code=$(printf '%s\n' "$hdrs" | awk '/^HTTP/ {c = $2} END {print c + 0}')
        remote=$(printf '%s\n' "$hdrs" | awk 'tolower($1) == "content-length:" {v = $2} END {print v}')
        local_size=$(stat -c %s "$SRC/$name" 2>/dev/null || echo "?")
        if [ "$code" = 200 ] && [ "$remote" = "$local_size" ]; then
            printf '  ok    %-40s %s bytes\n' "$name" "$remote"
        else
            printf '  FAIL  %-40s http %s, %s bytes remote vs %s local\n' \
                "$name" "$code" "${remote:-?}" "$local_size"
            fail=1
        fi
    done <<< "$sums"
    [ "$fail" = 0 ] && echo "all objects are publicly readable and the right size" \
        || { echo "one or more objects are not fetchable" >&2; exit 1; }
    echo
    echo "Sizes only. To prove the bytes end to end, download and run"
    echo "sha256sum -c SHA256SUMS against the result."
    exit 0
fi

[ -f "$SRC/tts_b6369a24.safetensors" ] || {
    echo "no weights in $SRC -- the large files are gitignored, so a fresh" >&2
    echo "clone has to repopulate them (see README.md)" >&2
    exit 1
}

echo "verifying checksums before publishing anything"
( cd "$SRC" && sha256sum -c SHA256SUMS )

IMMUTABLE="public, max-age=31536000, immutable"

# Two passes, because the binary payload and the text that documents it want
# different content types and a browser should be able to read the latter.
echo "uploading weights and embeddings"
aws s3 sync "$SRC" "s3://$BUCKET/$PREFIX" \
    --exclude "NOTICE" --exclude "SHA256SUMS" \
    --content-type "application/octet-stream" \
    --cache-control "$IMMUTABLE" \
    --size-only

echo "uploading NOTICE and SHA256SUMS"
aws s3 sync "$SRC" "s3://$BUCKET/$PREFIX" \
    --exclude "*" --include "NOTICE" --include "SHA256SUMS" \
    --content-type "text/plain; charset=utf-8" \
    --cache-control "$IMMUTABLE" \
    --size-only

cat <<EOF

Uploaded to s3://$BUCKET/$PREFIX

The objects still have to be publicly readable for the engine to fetch them
without credentials. Most buckets now block ACLs, so this is a bucket policy
rather than --acl public-read:

  {
    "Version": "2012-10-17",
    "Statement": [{
      "Sid": "PublicReadModelAssets",
      "Effect": "Allow",
      "Principal": "*",
      "Action": "s3:GetObject",
      "Resource": "arn:aws:s3:::$BUCKET/$PREFIX/*"
    }]
  }

Scope the Resource to the prefix, not the whole bucket, so publishing a model
never makes anything else in the bucket world-readable by accident.

Then check an anonymous fetch really works, which is the only test that
matches what a user's machine does:

  ./upload.sh --verify $BUCKET $PREFIX
EOF
