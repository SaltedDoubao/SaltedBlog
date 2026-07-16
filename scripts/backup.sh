#!/usr/bin/env bash
# ============================================
# SaltedBlog 备份脚本：数据库 + 上传图片 打包为 zip
# （与后台「备份管理」同一格式，可互相列表/恢复）
#
# 用法：
#   ./scripts/backup.sh            # 自动检测（docker postgres 运行中则备 PG，否则备本地 SQLite）
#   ./scripts/backup.sh sqlite     # 强制备份本地 SQLite（blog.db + uploads）
#   ./scripts/backup.sh postgres   # 强制备份 docker compose 中的 PostgreSQL + uploads 卷
#
# 环境变量：
#   BACKUP_DIR   备份输出目录（默认 backups/）
#   KEEP         保留最近 N 份（默认 7）
#   SQLITE_PATH  SQLite 文件路径（默认 data/blog.db）
#   UPLOAD_DIR   本地 uploads 目录（默认 data/uploads）
#
# 定时任务示例（每天凌晨 3 点）：
#   0 3 * * * cd /opt/SaltedBlog && ./scripts/backup.sh >> backups/backup.log 2>&1
# ============================================
set -euo pipefail

cd "$(dirname "$0")/.."

BACKUP_DIR="${BACKUP_DIR:-backups}"
KEEP="${KEEP:-7}"
SQLITE_PATH="${SQLITE_PATH:-data/blog.db}"
UPLOAD_DIR="${UPLOAD_DIR:-data/uploads}"
STAMP="$(date +%Y%m%d_%H%M%S)"
MODE="${1:-auto}"

mkdir -p "$BACKUP_DIR"
TMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TMP_DIR"' EXIT

detect_mode() {
    if [ -f deploy/docker-compose.yml ] && command -v docker >/dev/null 2>&1; then
        if [ -n "$(cd deploy && docker compose ps --status running -q postgres 2>/dev/null)" ]; then
            echo postgres
            return
        fi
    fi
    echo sqlite
}

backup_sqlite() {
    if [ ! -f "$SQLITE_PATH" ]; then
        echo "[backup] SQLite 数据库不存在: $SQLITE_PATH" >&2
        exit 1
    fi
    echo "[backup] 备份 SQLite: $SQLITE_PATH"
    if command -v sqlite3 >/dev/null 2>&1; then
        sqlite3 "$SQLITE_PATH" "VACUUM INTO '$TMP_DIR/blog.db'"
    else
        echo "[backup] 未安装 sqlite3 CLI，直接复制文件（建议先停服务）"
        cp "$SQLITE_PATH" "$TMP_DIR/blog.db"
    fi
    if [ -d "$UPLOAD_DIR" ]; then
        echo "[backup] 备份 uploads 目录"
        cp -r "$UPLOAD_DIR" "$TMP_DIR/uploads"
    else
        mkdir -p "$TMP_DIR/uploads"
    fi
}

backup_postgres() {
    echo "[backup] 备份 docker compose PostgreSQL"
    (cd deploy && docker compose exec -T postgres \
        sh -c 'pg_dump -U "$POSTGRES_USER" --no-owner --no-acl "$POSTGRES_DB"') > "$TMP_DIR/blog.sql"
    echo "[backup] 备份 uploads 卷"
    mkdir -p "$TMP_DIR/uploads"
    (cd deploy && docker compose cp api:/data/uploads/. "$TMP_DIR/uploads/") \
        || echo "[backup] uploads 卷复制失败（api 容器未运行？）"
}

write_manifest() {
    local engine="$1"
    cat > "$TMP_DIR/manifest.json" <<EOF
{
  "format_version": 1,
  "created_at": "$(date -u +%Y-%m-%dT%H:%M:%SZ)",
  "engines": ["${engine}"],
  "app": "saltedblog"
}
EOF
}

if [ "$MODE" = "auto" ]; then
    MODE="$(detect_mode)"
fi

case "$MODE" in
    sqlite) backup_sqlite ;;
    postgres) backup_postgres ;;
    *) echo "unknown mode: $MODE (sqlite|postgres|auto)" >&2; exit 1 ;;
esac

write_manifest "$MODE"

ARCHIVE="$BACKUP_DIR/saltedblog_${MODE}_${STAMP}.zip"
ARCHIVE_ABS="$(pwd)/$ARCHIVE"
if command -v zip >/dev/null 2>&1; then
    (cd "$TMP_DIR" && zip -r -0 "$ARCHIVE_ABS" . >/dev/null)
else
    # 无 zip 命令时用 Python 标准库
    python3 - "$TMP_DIR" "$ARCHIVE_ABS" <<'PY'
import sys, zipfile, os
src, dest = sys.argv[1], sys.argv[2]
with zipfile.ZipFile(dest, "w", compression=zipfile.ZIP_STORED) as zf:
    for root, dirs, files in os.walk(src):
        for name in files:
            path = os.path.join(root, name)
            zf.write(path, os.path.relpath(path, src))
PY
fi
echo "[backup] 完成: $ARCHIVE ($(du -h "$ARCHIVE_ABS" | cut -f1))"

ls -1t "$BACKUP_DIR"/saltedblog_*.zip 2>/dev/null | tail -n +$((KEEP + 1)) | while read -r old; do
    echo "[backup] 清理过期备份: $old"
    rm -f "$old"
done
