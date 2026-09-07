#!/usr/bin/env bash
# Run kobold with credentials passed directly or retrieved from a secret store.
#
# Usage:
#   export LLM_API_KEY="sk-..."
#   ./run.sh "your prompt"
#
#   # Or with AWS SSM Parameter Store:
#   OPENAI_KEY_PARAM=/kobold/openai-api-key ./run.sh "your prompt"
set -euo pipefail

PROFILE=${KOBOLD_PROFILE:-release}

here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
bin="$here/target/$PROFILE/kobold"

if [ ! -x "$bin" ]; then
  build_args=(build)
  [ "$PROFILE" = release ] && build_args+=(--release)
  cargo "${build_args[@]}" --manifest-path "$here/Cargo.toml"
fi

# 1. Use ambient LLM_API_KEY or OPENAI_API_KEY if already set
KEY="${LLM_API_KEY:-${OPENAI_API_KEY:-}}"

# 2. If unset, check if AWS SSM parameter store is configured
if [ -z "$KEY" ] && [ -n "${OPENAI_KEY_PARAM:-}" ]; then
  PARAM="$OPENAI_KEY_PARAM"
  REGION="${AWS_REGION:-us-east-2}"
  if ! KEY=$(aws ssm get-parameter \
        --name "$PARAM" \
        --with-decryption \
        --region "$REGION" \
        --query Parameter.Value \
        --output text 2>&1); then
    # Never let a failure message echo the value back.
    printf 'could not read %s from SSM (region %s)\n' "$PARAM" "$REGION" >&2
    printf '%s\n' "$KEY" | sed -E 's/[A-Za-z0-9_-]{20,}/<redacted>/g' >&2
    exit 1
  fi
fi

if [ -z "$KEY" ] || [ "$KEY" = "None" ]; then
  printf 'Error: LLM_API_KEY (or OPENAI_API_KEY) is not set.\n' >&2
  printf 'Provide it via environment:\n  export LLM_API_KEY="sk-..."\n' >&2
  exit 1
fi

exec env LLM_API_KEY="$KEY" "$bin" "$@"
