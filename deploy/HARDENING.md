# 生产安全上线清单

## 网络边界

1. `SITE_DOMAIN` 只提供公开站点；Caddy 会对 `/admin`、`/api/admin/*`、`/api/auth/*` 返回 404。
2. `ADMIN_DOMAIN` 使用私有 DNS 指向 VPN 地址，必须绕过公网外层代理。`ADMIN_ALLOWED_CIDRS` 必须填写实际 VPN 客户端网段；Caddy 仅接受这些来源并使用内部 CA HTTPS。
3. 将 Caddy 内部 CA 根证书安全安装到管理设备。根证书位于 `caddy_data` 卷，禁止公开下载。
4. 宿主机防火墙只开放公网 80/443、VPN 端口和受限来源的 SSH；SSH 禁止 root/密码登录。
5. `UPSTREAM_PROXY_CIDRS` 填写外层代理的精确 CIDR；`UPSTREAM_PROXY_CIDRS` 与 `ADMIN_ALLOWED_CIDRS` 均不得使用 `0.0.0.0/0`。API 只信任固定地址 `172.30.0.10/32` 上的 Caddy。

## 首次部署与升级

```bash
cd deploy
mkdir -p secrets
# 按 secrets/README.md 创建全部 secret 文件
docker compose config
docker compose up -d --build
```

`db-init` 会幂等创建 `salted_owner` 与 `salted_app`；`migrate` 使用 owner 角色执行 DDL，API
仅使用运行时角色。升级时仍执行 `docker compose up -d --build`，API 会在迁移任务完成后启动。
上传和备份共用 `app_data` 卷，以保证恢复时可在同一文件系统内旁路解压并原子切换上传目录。

首次登录必须在 5 分钟内完成 TOTP 绑定并离线保存 10 个恢复码。丢失第二因子时在服务器执行：

```bash
docker compose run --rm migrate salted-api reset-mfa admin
```

## 旧数据兼容

- 安全迁移会撤销全部旧会话，并重新净化所有已保存文章 HTML。
- 旧 SVG/ICO/GIF/AVIF 文件不会删除，但 `/uploads` 只再提供 JPEG/PNG/WebP；请在素材页重新上传替换。
- 网页只接受带当前实例 HMAC 签名的 v2 备份。旧 v1 备份应离线保存，不可直接上传执行。

## 宿主机

- 使用受支持的 Linux 发行版，启用自动安全更新、磁盘加密和 Docker Rootless（条件允许时）。
- 定期将加密后的备份复制到异机；每季度在隔离环境验证恢复。
- 检查 `caddy_logs` 和容器 JSON 日志轮转；生产告警应至少覆盖登录失败激增、CSRF/SSRF 拦截和连续 5xx。
