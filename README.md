# SaltedBlog

「明日方舟：终末地」工业风格的中英双语个人博客。Astro SSR 前台 + 自建后台，Rust Axum API，SQLite / PostgreSQL 双数据库支持，Docker Compose 一键部署。

## 架构

```text
浏览器
  │
Caddy（自动 HTTPS）
  ├── /            → web  (Astro SSR, Node)   前台页面 + /admin 后台
  ├── /api/*       → api  (Rust Axum)         业务接口
  └── /uploads/*   → api                      上传的图片
                      │
                      ├── SQLite（本地开发，零依赖）/ PostgreSQL（Docker 部署）
                      └── data/uploads/       图片文件
```

## 功能

- 文章：Markdown 写作、发布/草稿、分类、标签、系列（专栏）、封面、摘要
- 中英双语：界面双语（`/` 中文、`/en/` 英文），每篇文章可选关联另一语言版本，自动输出 hreflang
- 阅读体验：目录（TOC）滚动高亮、代码高亮 + 一键复制、深浅双主题、系列导航、上下篇
- 站点：归档时间线、全文搜索（jieba 中文分词）、RSS（中英双份）、sitemap、SEO meta/OG
- 评论：Giscus（基于 GitHub Discussions，后台可配置，主题跟随深浅模式）
- 统计：自建轻量 PV/UV 统计（按日去重），文章阅读量，后台 30 天趋势图表
- 后台 `/admin`：登录（argon2 + Session + 登录限流）、仪表盘、文章管理（CodeMirror 分屏编辑器、
  图片粘贴/拖拽上传）、分类/标签/系列、友链、图片素材库、站点设置
- 运维：Docker Compose（Caddy + Web + API + PostgreSQL）、备份脚本（数据库 + 图片打包）

## 目录结构

```text
SaltedBlog/
├── web/                  # Astro SSR：前台 + /admin 后台
│   └── src/
│       ├── pages/        # 路由（en/ 下为英文镜像；admin/ 为后台）
│       ├── components/   # 组件与共享页面实现
│       ├── layouts/      # BaseLayout（前台）/ AdminLayout（后台守卫）
│       ├── styles/       # 设计令牌 tokens.css / base.css / prose.css / admin.css
│       ├── lib/          # API 客户端、RSS/sitemap 构建、后台前端工具
│       └── i18n/         # 中英 UI 字典与路径工具
├── api/                  # Rust Axum + SeaORM
│   ├── migration/        # 数据库迁移（SQLite / PG 通用）
│   └── src/
│       ├── entities/     # SeaORM 实体
│       ├── routes/       # public / auth / admin 路由
│       ├── auth.rs       # argon2、Session、登录限流、守卫中间件
│       └── render.rs     # comrak 渲染 + TOC 提取 + jieba 分词
├── deploy/               # Dockerfile.api / Dockerfile.web / docker-compose.yml / Caddyfile
├── scripts/backup.sh     # 备份脚本
├── data/                 # 运行时数据（gitignore）：blog.db、uploads/
└── .env.example
```

## 本地开发（Windows / macOS / Linux）

依赖：Node.js 22+、Rust 1.8x+（本项目在 1.96 上开发）。本地默认使用 SQLite，无需装数据库。

```bash
# 1. 环境变量（根目录）
cp .env.example .env        # 修改 ADMIN_PASSWORD

# 2. 启动 Rust API（终端 1）——首次运行自动建库、迁移、创建管理员
cd api
cargo run

# 3. 启动 Astro（终端 2）
cd web
npm install
npm run dev
```

- 前台：<http://localhost:4321>（dev 服务器已将 `/api`、`/uploads` 代理到 8787）
- 后台：<http://localhost:4321/admin>，账号密码见 `.env` 的 `ADMIN_USERNAME` / `ADMIN_PASSWORD`
- 管理员账号只在 `users` 表为空时自动创建；改密码可清空 `users` 表后重启 API

## 部署（Docker Compose）

服务器要求：Docker 24+，域名已解析到服务器（Caddy 自动申请 HTTPS 证书）。

```bash
git clone <repo> /opt/SaltedBlog && cd /opt/SaltedBlog/deploy
cp ../.env.example .env
# 必改：SITE_DOMAIN、POSTGRES_PASSWORD、ADMIN_PASSWORD
docker compose up -d --build
```

更新版本：`git pull && docker compose up -d --build`。

数据落在 named volume：`pg_data`（数据库）、`uploads`（图片）、`caddy_data`（证书）。

## 备份与恢复

```bash
./scripts/backup.sh              # 自动检测模式，输出到 backups/saltedblog_*.tar.gz
KEEP=30 ./scripts/backup.sh      # 保留最近 30 份
```

定时备份（crontab）：`0 3 * * * cd /opt/SaltedBlog && ./scripts/backup.sh >> backups/backup.log 2>&1`

恢复：

```bash
# SQLite：解包后把 blog.db 放回 data/，uploads/ 放回 data/uploads
tar -xzf backups/saltedblog_sqlite_xxx.tar.gz -C /tmp/restore

# PostgreSQL：
tar -xzf backups/saltedblog_postgres_xxx.tar.gz -C /tmp/restore
cd deploy
cat /tmp/restore/blog.sql | docker compose exec -T postgres \
  sh -c 'psql -U "$POSTGRES_USER" "$POSTGRES_DB"'
docker compose cp /tmp/restore/uploads/. api:/data/uploads
```

## 主要环境变量

| 变量 | 说明 | 默认 |
|---|---|---|
| `DATABASE_URL` | `sqlite://data/blog.db?mode=rwc` 或 `postgres://…` | SQLite |
| `ADMIN_USERNAME` / `ADMIN_PASSWORD` | 首次启动引导创建的管理员 | admin / 空 |
| `API_URL` | Web SSR 访问 API 的内部地址 | `http://127.0.0.1:8787` |
| `PUBLIC_SITE_URL` | 站点对外地址（canonical / RSS / sitemap） | `http://localhost:4321` |
| `SITE_DOMAIN` | 域名（仅 Docker，供 Caddy 使用） | — |
| `UPLOAD_MAX_MB` | 上传大小上限 | 20 |
| `STATS_TZ_OFFSET_HOURS` | 统计时区偏移（北京=8） | 8 |

## Giscus 评论配置

1. 博客仓库（或任意公开仓库）开启 Discussions，安装 [giscus app](https://github.com/apps/giscus)
2. 到 <https://giscus.app/zh-CN> 生成参数（repo、repoId、category、categoryId）
3. 填入后台「站点设置 → GISCUS」，保存后文章页自动出现评论区

## 设计语言

视觉规范参照《明日方舟：终末地》官网风格：终末黄 `#F4F600` 信号色、黑白冷灰高对比、
硬边框 2px 圆角、切角按钮、编号装饰（`01 / NEWS`）、网格纹理、克制动效，
中文使用 HarmonyOS Sans 系统栈，英文展示/数据字体为自托管 Space Grotesk。
