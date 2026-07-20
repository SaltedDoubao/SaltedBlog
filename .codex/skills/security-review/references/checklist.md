# SaltedBlog 安全审查清单

## 身份与浏览器边界

- 管理接口仍要求已认证会话、合法 Origin/CSRF，并在高危操作上要求 step-up。
- 登录限流不能只依赖可伪造的转发头；Cookie 属性、闲置/绝对超时和会话撤销保持有效。
- MFA 密钥、恢复码和密码只以加密或单向形式保存，错误信息不泄露账号或密钥状态。

## 文件与数据边界

- 上传、备份和恢复拒绝绝对路径、`..`、符号链接逃逸、超限内容及不受支持格式。
- 恢复前备份、完整性/HMAC 校验和 SQLite/PostgreSQL 引擎匹配不能绕过。
- CSV/JSONL 导出避免公式注入和秘密字段；日志不包含 Cookie、Authorization、CSRF、数据库 URL 或 LLM Key。

## 网络与部署边界

- 出站 HTTP 只允许 HTTP(S) 公网目标，解析后仍阻断私网、回环、链路本地和重定向逃逸。
- 公网域名继续对 `/admin`、`/api/admin/*`、`/api/auth/*` 返回 404；管理域只信任明确的 VPN/Tunnel 来源。
- 生产秘密只通过 Docker Secrets/`*_FILE` 读取；容器保持非 root、只读根文件系统和最小权限。
