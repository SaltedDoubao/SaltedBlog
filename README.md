# SaltedBlog

「明日方舟：终末地」工业风格的双语 UI 个人博客。Astro SSR 前台 + 自建后台，Rust Axum API，SQLite / PostgreSQL 双数据库支持，Docker Compose 一键部署。

## 架构

```text
浏览器
  │
Caddy（自动 HTTPS）
  ├── 公网域名 /   → web  (Astro SSR, Node)   前台页面（后台路径统一 404）
  ├── VPN 管理域名 → web  (Astro SSR, Node)   /admin 后台
  ├── /api/*       → api  (Rust Axum)         业务接口
  └── /uploads/*   → api                      上传的图片
                      │
                      ├── SQLite（本地开发，零依赖）/ PostgreSQL（Docker 部署）
                      └── data/uploads/       图片文件
```

## 功能

- 文章：Markdown 写作、发布/草稿、分类、标签、系列（专栏）、封面、摘要
- 双语 UI：`/` 使用中文界面、`/en/` 使用英文界面，两套界面共享同一文章集合，正文可自由使用任意语言
- 阅读体验：目录（TOC）滚动高亮、代码高亮 + 一键复制、深浅双主题、系列导航、上下篇
- 站点：归档时间线、全文搜索（jieba 中文分词）、RSS（中英双份）、sitemap、SEO meta/OG
- 评论：Giscus（基于 GitHub Discussions，后台可配置，主题跟随深浅模式）
- 统计：自建轻量 PV/UV 统计（按日去重），文章阅读量，后台 30 天趋势图表
- AI 情报日报：内置多信源聚合管线（RSS/Atom + GitHub Trending → 指纹去重 → 关键词过滤 →
  权重配额选稿 → LLM 中文整理），每日自动生成单篇中文「AI 前沿日报」文章（分类 `ai-daily`），
  主页「最新情报」通过双卡轮播与黄色滚动字幕展示最新一期重点条目
- 后台 `/admin`：VPN 私网入口、登录（argon2 + 强制 TOTP + CSRF + 登录限流）、仪表盘、日志中心、文章管理（CodeMirror 分屏编辑器、
  图片粘贴/拖拽上传）、情报管理（信源 CRUD/试抓/采集日志/条目审计/日报任务与 LLM 配置）、
  分类/标签/系列、友链、图片素材库、站点设置、备份管理（生成/恢复/上传/下载）
- 运维：Docker Compose（Caddy + Web + API + PostgreSQL）、备份脚本与后台共用 zip 格式（数据库 + 图片）

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
├── deploy/               # Dockerfile / Compose / Caddyfile / 部署 .env.example
├── scripts/backup.sh     # 备份脚本（与后台同格式 zip）
├── data/                 # 运行时数据（gitignore）：blog.db、uploads/
├── backups/              # 备份 zip（gitignore）
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
git clone <repo> /opt/SaltedBlog && cd /opt/SaltedBlog
cp deploy/.env.example deploy/.env
# 按 deploy/secrets/README.md 创建生产密钥，并配置域名、CIDR 与已发布版本
# SALTEDBLOG_IMAGE_TAG=v0.1.5
docker compose --project-directory ./deploy --env-file ./deploy/.env \
  -f ./deploy/docker-compose.yml config --quiet
docker compose --project-directory ./deploy --env-file ./deploy/.env \
  -f ./deploy/docker-compose.yml pull
docker compose --project-directory ./deploy --env-file ./deploy/.env \
  -f ./deploy/docker-compose.yml up -d --no-build
```

所有 Docker Compose 命令均从仓库根目录执行，并显式指定 `deploy/.env`，因此不会读取本地开发使用的根目录 `.env`。
更新时先生成备份，再修改 `SALTEDBLOG_IMAGE_TAG`，重新执行上述 `pull` 与 `up` 命令。
回滚时将标签改回上一版本或 `sha-<完整提交 SHA>`。开发及应急场景仍可将最后一条命令改为 `up -d --build`。

推送 `v*` Git 标签后，GitHub Actions 会在测试通过后发布 `linux/amd64` 的 API、Web、Caddy
镜像到 GHCR。首次发布后，需要在 GitHub Packages 中将三个容器包设为 public，VPS 才能免登录拉取。

数据落在 named volume：`pg_data`（数据库）、`uploads`（图片）、`caddy_data`（证书）。

## 备份与恢复

备份内容为**当前数据库引擎**对应的库文件（SQLite → `blog.db`，PostgreSQL → `blog.dump`）+ `uploads/`。v2 备份包含逐文件 SHA-256 与实例 HMAC 签名，SQLite 与 PostgreSQL **不互相转换恢复**。

### 后台「备份管理」

登录 `/admin/backup` 可生成备份、上传本机 zip、下载、恢复、删除。恢复前会自动再生成一份当前状态的安全备份。默认保留最近 7 份（`BACKUP_KEEP`）。

### 命令行脚本

```bash
./scripts/backup.sh              # 调用应用生成带签名的 v2 备份
```

定时备份（crontab）：`0 3 * * * cd /opt/SaltedBlog && ./scripts/backup.sh >> backups/backup.log 2>&1`

zip 内部结构：

```text
manifest.json
blog.db          # 仅 SQLite 备份
blog.dump        # 仅 PostgreSQL custom dump
uploads/
```

恢复也可在后台完成；若手动恢复：

```bash
# 将 zip 放到 backups/ 后，在后台选择「恢复」
# 或对本机 SQLite：解压后替换 data/blog.db 与 data/uploads/
```

相关环境变量：`BACKUP_DIR`（默认 `backups`）、`BACKUP_KEEP`（默认 7）、`BACKUP_UPLOAD_MAX_MB`（默认 1024）。
## 主要环境变量

| 变量 | 说明 | 默认 |
|---|---|---|
| `DATABASE_URL` | `sqlite://data/blog.db?mode=rwc` 或 `postgres://…` | SQLite |
| `ADMIN_USERNAME` / `ADMIN_PASSWORD` | 首次启动引导创建的管理员 | admin / 空 |
| `API_URL` | Web SSR 访问 API 的内部地址 | `http://127.0.0.1:8787` |
| `PUBLIC_SITE_URL` | 站点对外地址（canonical / RSS / sitemap） | `http://localhost:4321` |
| `SALTEDBLOG_IMAGE_TAG` | GHCR 镜像版本标签（仅 Docker） | 由 `deploy/.env` 指定 |
| `SITE_DOMAIN` | 域名（仅 Docker，供 Caddy 使用） | 由 `deploy/.env` 指定 |
| `ADMIN_DOMAIN` / `ADMIN_ORIGIN` | VPN 管理域名与唯一合法 Origin | Docker 由 `deploy/.env` 指定 |
| `UPLOAD_MAX_MB` | 上传大小上限 | 20 |
| `BACKUP_DIR` | 备份目录 | `backups` |
| `BACKUP_KEEP` | 自动保留最近 N 份备份 | 7 |
| `BACKUP_UPLOAD_MAX_MB` | 后台上传备份 zip 上限（MB） | 1024 |
| `SESSION_IDLE_MINUTES` / `SESSION_ABSOLUTE_HOURS` | 会话闲置/绝对超时 | 30 / 12 |
| `MFA_REQUIRED` / `MFA_ENCRYPTION_KEY` | TOTP 强制开关与密钥 | false / 空 |
| `BACKUP_SIGNING_KEY` | v2 备份签名密钥 | 空 |
| `STATS_TZ_OFFSET_HOURS` | 统计时区偏移（北京=8） | 8 |
| `NEWS_LLM_API_KEY` | AI 日报 LLM 的 API Key（OpenAI 兼容，不落库） | 空 |

生产环境使用 Docker Secrets、最小权限 PostgreSQL 双角色和独立迁移任务。传统 VPN/私有 DNS 部署见
[`deploy/HARDENING.md`](deploy/HARDENING.md)；希望从任意浏览器安全访问后台时，使用
[`deploy/CLOUDFLARE_ACCESS.md`](deploy/CLOUDFLARE_ACCESS.md) 中的 Cloudflare Tunnel + Access 方案。

## AI 情报日报

1. `.env` 配置 `NEWS_LLM_API_KEY`，重启 API
2. 后台「情报管理」填写 LLM Base URL（如 `https://api.deepseek.com/v1`）与模型名，保存配置
3. 点「立即采集全部」验证信源可用（预置 HN / GitHub Trending / arXiv / InfoQ / V2EX，可增删改）
   - V2EX 预置地址依赖其上游 RSS 输出；若日志显示「正文为空」，表示上游暂未返回 Feed，保留信源后会在上游恢复时自动继续采集。
4. 在「任务列表」中检查升级时生成的两条默认任务；它们默认关闭，可分别编辑并点击「立即执行」试跑
5. 信息采集任务可配置起始时间与 1～24 小时采集间隔；整理发布任务可配置日报标题、生成时间及生成后处理
   - 每条任务拥有独立开关；可创建多条任务，采集任务均处理全部已启用信源
   - 整理任务可选择始终保存草稿，或在晚于生成时间的指定分钟自动发布；每条任务生成独立中文日报
   - 所有计划时点仅在对应分钟触发，服务停机或错过时点不会补跑；当天失败可从任务列表强制执行并覆盖该任务当天文章
   - 手动执行不改变自动调度节奏，禁用任务不会继续自动生成或发布
6. 数据保留：原始条目默认 30 天、采集日志 7 天，自动清理，阈值均可在后台调整

## Giscus 评论配置

1. 博客仓库（或任意公开仓库）开启 Discussions，安装 [giscus app](https://github.com/apps/giscus)
2. 到 <https://giscus.app/zh-CN> 生成参数（repo、repoId、category、categoryId）
3. 填入后台「站点设置 → GISCUS」，保存后文章页自动出现评论区

## 设计语言

视觉规范参照《明日方舟：终末地》官网风格：终末黄 `#F4F600` 信号色、黑白冷灰高对比、
硬边框 2px 圆角、切角按钮、编号装饰（`01 / NEWS`）、网格纹理、克制动效，
中文使用 HarmonyOS Sans 系统栈，英文展示/数据字体为自托管 Space Grotesk。
