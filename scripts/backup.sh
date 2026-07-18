#!/bin/sh
set -eu

# 使用应用自身的 v2 签名备份实现，避免脚本生成无法由后台验证的旧格式。
ROOT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)

if command -v docker >/dev/null 2>&1 && [ -f "$ROOT_DIR/deploy/docker-compose.yml" ]; then
  exec docker compose \
    --project-directory "$ROOT_DIR/deploy" \
    --env-file "$ROOT_DIR/deploy/.env" \
    -f "$ROOT_DIR/deploy/docker-compose.yml" \
    exec -T api salted-api backup
fi

cd "$ROOT_DIR/api"
exec cargo run --quiet -- backup
