# SaltedBlog 工程规则

## 通用约定

- 始终使用简体中文回复。
- 读取包含中文的文件前先执行 `chcp 65001`。
- 尊重已有工作树，不覆盖用户改动；只修改当前任务涉及的文件。
- 不提交 `.env`、`deploy/secrets/*`、数据库、上传、备份、日志、构建产物或 Python 字节码。
- 普通提交以本地门禁通过为完成条件，不依赖 GitHub Actions。只有修改根目录 `VERSION` 的版本提交才触发远端发布预检。

## 项目边界

- `api/`：Rust Axum API、SeaORM 实体与 SQLite/PostgreSQL 迁移。
- `web/`：Astro SSR 前台和管理后台。
- `deploy/`：生产 Dockerfile、Compose、Caddy、初始化脚本与部署文档。
- `.codex/skills/`：可重复执行的项目工作流和确定性校验脚本。
- `data/`、`backups/`、根 `.env` 与 `deploy/secrets/`：本地或生产敏感状态，禁止纳入版本控制。

## 工作流路由

| 变更类型 | 必须使用的技能 | 最低验证 |
|---|---|---|
| 任意源码、配置或文档 | `change-validation` | 按变更路径执行本地门禁 |
| `api/migration/`、实体或持久化结构 | `migration-safety` | 历史迁移不可变、注册检查、SQLite 与 PostgreSQL 迁移 |
| 认证、MFA、CSRF、上传、备份恢复、日志、出站请求、代理或秘密 | `security-review` | 威胁检查清单、相关回归测试、通用 API 门禁 |
| Dockerfile、Compose、Caddy 或生产环境变量 | `deployment-preflight` | Compose/Caddy 校验、相关镜像与运行时检查 |
| `VERSION`、版本升级、标签、镜像或 GitHub Release | `release-preflight` | 完整本地预检、同 SHA 远端预检、注释标签 |

跨域变更执行所有相关技能，验证项取并集。API 响应或请求结构改变时，同步 Rust DTO、Web 类型、调用方和测试；不得依赖运行时猜测旧字段。

## 稳定性不变量

- 已包含在任一 `v*` 标签中的迁移文件只增不改；新迁移必须同时支持 SQLite 与 PostgreSQL，并在 `migration/src/lib.rs` 中按顺序注册一次。
- 生产容器保持非 root、只读文件系统、最小 capability、健康检查和 Docker Secrets；不得把秘密写入镜像层、Compose 环境值、日志或错误响应。
- 认证与管理接口保持 CSRF、Origin、会话超时、MFA 和登录限流边界；上传/恢复路径必须防遍历，出站请求必须继续阻断私网与危险重定向。
- Docker 或 PostgreSQL 不可用时，不得把部署、迁移完整预检或发布任务报告为通过。
- 不创建、移动、删除或强推失败的发布标签。修复后升级补丁版本并重新完成全流程。

## 日常变更

从仓库根目录运行：

```powershell
python .codex/skills/change-validation/scripts/validate_change.py --scope auto
```

默认检查暂存、未暂存和未跟踪文件；已提交改动使用 `--base <ref>`。提交前必须报告实际执行的检查及结果。

## 发布流程

1. 使用 `release-preflight` 的版本脚本更新 `VERSION` 及所有版本镜像。
2. 提交版本变更前运行完整本地预检；Docker 不可用则停止，不得打标签。
3. 将版本提交推送到 `main`，确认该提交 SHA 的 `release-preflight` Actions 成功。
4. 仅为该 SHA 创建并推送 `v<VERSION>` 注释标签，随后确认 `release-images` 成功。
5. 标签构建失败时保留原标签，修复后升级补丁版本并重新执行以上步骤。
