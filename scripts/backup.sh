#!/usr/bin/env bash
# ============================================
# SaltedBlog 备份脚本：数据库 + 上传图片 打包为 tar.gz
#
# 用法：
#   ./scripts/backup.sh            # 自动检测（docker postgres 运行中则备 PG，否则备本地 SQLite）
#   ./scripts/backup.sh sqlite     # 强制备份本地 SQLite（data/blog.db + data/uploads）
#   ./scripts/backup.sh postgres   # 强制备份 docker compose 中的 PostgreSQL + uploads 卷
#
# 环境变量：
#   BACKUP_DIR   备份输出目录（默认 backups/）
#   KEEP         保留最近 N 份（默认 14）
#   SQLITE_PATH  SQLite 文件路径（默认 data/blog.db）
#
# 定时任务示例（每天凌晨 3 点）：
#   0 3 * * * cd /opt/SaltedBlog && ./scripts/backup.sh >> backups/backup.log 2>&1
# ============================================
set -euo pipefail

cd "$(dirname "$0")/.."

BACKUP_DIR="${BACKUP_DIR:-backups}"
KEEP="${KEEP:-14}"
SQLITE_PATH="${SQLITE_PATH:-data/blog.db}"
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
        # VACUUM INTO 产生一致性快照，运行中也安全
        sqlite3 "$SQLITE_PATH" "VACUUM INTO '$TMP_DIR/blog.db'"
    else
        echo "[backup] 未安装 sqlite3 CLI，直接复制文件（建议先停服务）"
        cp "$SQLITE_PATH" "$TMP_DIR/blog.db"
    fi
    if [ -d data/uploads ]; then
        echo "[backup] 备份 uploads 目录"
        cp -r data/uploads "$TMP_DIR/uploads"
    fi
}

backup_postgres() {
    echo "[backup] 备份 docker compose PostgreSQL"
    (cd deploy && docker compose exec -T postgres \
        sh -c 'pg_dump -U "$POSTGRES_USER" "$POSTGRES_DB"') > "$TMP_DIR/blog.sql"
    echo "[backup] 备份 uploads 卷"
    (cd deploy && docker compose cp api:/data/uploads "$TMP_DIR/uploads") \
        || echo "[backup] uploads 卷复制失败（api 容器未运行？）"
}

if [ "$MODE" = "auto" ]; then
    MODE="$(detect_mode)"
fi

case "$MODE" in
    sqlite) backup_sqlite ;;
    postgres) backup_postgres ;;
    *) echo "unknown mode: $MODE (sqlite|postgres|auto)" >&2; exit 1 ;;
esac

ARCHIVE="$BACKUP_DIR/saltedblog_${MODE}_${STAMP}.tar.gz"
tar -czf "$ARCHIVE" -C "$TMP_DIR" .
echo "[backup] 完成: $ARCHIVE ($(du -h "$ARCHIVE" | cut -f1))"

# 只保留最近 KEEP 份
ls -1t "$BACKUP_DIR"/saltedblog_*.tar.gz 2>/dev/null | tail -n +$((KEEP + 1)) | while read -r old; do
    echo "[backup] 清理过期备份: $old"
    rm -f "$old"
done
