# Ubuntu + Docker + Cloudflare Access 部署指南

本文从一台新 Ubuntu 24.04 LTS 服务器开始，部署 SaltedBlog，并通过 Cloudflare Tunnel + Access
实现“任意浏览器访问后台、无需 VPN 客户端”。

最终入口：

- 公开网站：`https://blog.example.com`
- 管理后台：`https://admin.example.com/admin`
- 管理链路：浏览器 → Cloudflare Access → Cloudflare Tunnel → 内部 Caddy → SaltedBlog 密码与 TOTP

请将全文中的域名、邮箱、仓库地址和服务器 IP 替换为自己的值。命令默认使用具有 `sudo` 权限的普通用户，
不建议长期直接使用 root。

## 1. 准备域名与服务器

需要：

- 一台 Ubuntu 24.04 LTS 服务器，建议至少 2 核 CPU、2 GB 内存、20 GB 磁盘；
- 一个已添加到 Cloudflare 的域名；
- 两个子域名，例如 `blog.example.com` 和 `admin.example.com`；
- 一个仅自己控制、已开启 MFA 的邮箱或 Google/GitHub 身份账号。

在 Cloudflare DNS 中为公开站点创建 `A`/`AAAA` 记录，指向服务器公网地址。建议初次部署时将
`blog.example.com` 设为 **DNS only**，让 Caddy 直接签发证书。不要为 `admin.example.com` 创建指向
服务器公网 IP 的记录；Tunnel 发布路由时会为它创建专用记录。

## 2. 初始化 Ubuntu

```bash
sudo apt update
sudo apt full-upgrade -y
sudo apt install -y ca-certificates curl git openssl ufw
sudo timedatectl set-timezone Asia/Shanghai
```

配置最小防火墙：

```bash
sudo ufw default deny incoming
sudo ufw default allow outgoing
sudo ufw allow OpenSSH
sudo ufw allow 80/tcp
sudo ufw allow 443/tcp
sudo ufw enable
sudo ufw status verbose
```

Docker 发布端口可能绕过部分 UFW 规则，因此 Compose 只应发布项目明确需要的 80/443；PostgreSQL、
API、Web 和 cloudflared 均不得单独映射到宿主机端口。

## 3. 安装 Docker Engine 与 Compose

使用 Docker 官方 APT 仓库：

```bash
sudo install -m 0755 -d /etc/apt/keyrings
sudo curl -fsSL https://download.docker.com/linux/ubuntu/gpg \
  -o /etc/apt/keyrings/docker.asc
sudo chmod a+r /etc/apt/keyrings/docker.asc

sudo tee /etc/apt/sources.list.d/docker.sources >/dev/null <<EOF
Types: deb
URIs: https://download.docker.com/linux/ubuntu
Suites: $(. /etc/os-release && echo "${UBUNTU_CODENAME:-$VERSION_CODENAME}")
Components: stable
Architectures: $(dpkg --print-architecture)
Signed-By: /etc/apt/keyrings/docker.asc
EOF

sudo apt update
sudo apt install -y docker-ce docker-ce-cli containerd.io docker-buildx-plugin docker-compose-plugin
sudo systemctl enable --now docker
sudo docker run --rm hello-world
sudo docker compose version
```

Docker 官方安装说明：<https://docs.docker.com/engine/install/ubuntu/>。

## 4. 下载项目

```bash
sudo mkdir -p /opt/SaltedBlog
sudo chown "$USER":"$USER" /opt/SaltedBlog
git clone https://github.com/YOUR_ACCOUNT/SaltedBlog.git /opt/SaltedBlog
cd /opt/SaltedBlog
git status --short
```

生产部署应固定到经过验证的 tag 或 commit，而不是长期追踪未知的最新分支：

```bash
git checkout <已验证的-tag-或-commit>
```

## 5. 创建 Cloudflare Access 应用

必须先创建 Access 应用，再发布 Tunnel 路由，避免管理域名短暂裸露。

1. 进入 Cloudflare Zero Trust → **Access controls → Applications**。
2. 创建 **Self-hosted** 应用，域名填写 `admin.example.com`，保护整个 hostname。
3. 建立 `Allow` 策略：`Include → Emails → 你的完整邮箱地址`。
4. 不要使用 `Everyone` 或“所有有效邮箱”；个人站点应只允许明确列出的账号。
5. 选择邮箱一次性验证码，或接入已开启 MFA 的 Google/GitHub 身份提供商。
6. 会话时长建议设置为 8 小时；需要更方便时最多放宽到 24 小时。
7. 如果只使用一个身份提供商，可启用 Instant Auth。

Access 默认拒绝不匹配 Allow 策略的用户。官方说明：
<https://developers.cloudflare.com/cloudflare-one/access-controls/applications/http-apps/self-hosted-public-app/>。

## 6. 创建 Tunnel 并保存 Token

1. 进入 Zero Trust → **Networks → Connectors / Tunnels**。
2. 创建 remotely-managed Cloudflare Tunnel，例如 `saltedblog-admin`。
3. 选择 Docker 连接器，复制页面生成的 Tunnel Token。
4. 此时先不要创建无 Access 保护的公开路由。

在服务器创建 Secrets。数据库密码使用十六进制，避免 URL 转义问题：

```bash
cd /opt/SaltedBlog/deploy
mkdir -p secrets
chmod 700 secrets
umask 077

openssl rand -hex 24 > secrets/postgres_superuser_password
openssl rand -hex 24 > secrets/postgres_owner_password
openssl rand -hex 24 > secrets/postgres_app_password
openssl rand -base64 36 > secrets/admin_password
openssl rand -hex 32 > secrets/mfa_encryption_key
openssl rand -hex 32 > secrets/backup_signing_key
: > secrets/news_llm_api_key

APP_DB_PASSWORD=$(cat secrets/postgres_app_password)
OWNER_DB_PASSWORD=$(cat secrets/postgres_owner_password)
printf 'postgres://salted_app:%s@postgres:5432/saltedblog\n' "$APP_DB_PASSWORD" \
  > secrets/database_url
printf 'postgres://salted_owner:%s@postgres:5432/saltedblog\n' "$OWNER_DB_PASSWORD" \
  > secrets/database_maintenance_url
unset APP_DB_PASSWORD OWNER_DB_PASSWORD

read -rsp '粘贴 Cloudflare Tunnel Token：' CF_TUNNEL_TOKEN
printf '%s\n' "$CF_TUNNEL_TOKEN" > secrets/cloudflare_tunnel_token
unset CF_TUNNEL_TOKEN
echo
chmod 600 secrets/*
```

将 `mfa_encryption_key`、`backup_signing_key` 和管理员初始密码离线保存。丢失前两个密钥会分别导致
TOTP 数据无法解密、已签名备份无法通过网页恢复。Tunnel Token 泄露后应立即在 Cloudflare 撤销并轮换。

## 7. 配置生产环境

```bash
cd /opt/SaltedBlog/deploy
cp ../.env.example .env
nano .env
```

至少修改以下值：

```dotenv
SITE_DOMAIN=blog.example.com
ADMIN_DOMAIN=admin.example.com

# 只允许固定 cloudflared 容器进入内部管理站点。
ADMIN_ALLOWED_CIDRS=172.30.0.40/32

# 公开站点采用 DNS only、直接进入 Caddy 时使用此值。
UPSTREAM_PROXY_CIDRS=127.0.0.1/32

POSTGRES_SUPERUSER=salted_admin
POSTGRES_DB=saltedblog
BACKUP_KEEP=7
STATS_TZ_OFFSET_HOURS=8
```

生产 Compose 会自动设置：

```text
APP_ENV=production
COOKIE_SECURE=true
MFA_REQUIRED=true
ADMIN_ORIGIN=https://admin.example.com
```

不要在 `.env` 中再放一份生产密码；实际密码均从 `deploy/secrets/` 读取。

## 8. 发布 Tunnel 路由

回到 Cloudflare Tunnel 控制台，为 `saltedblog-admin` 添加 Published application route：

```text
Hostname: admin.example.com
Service:  http://caddy:8080
HTTP Host Header: admin.example.com
```

开启 **Protect with Access**，并选择第 5 步创建的 Access 应用。该设置让 cloudflared 在把请求交给
源站前验证 Access Token；项目内部 Caddy 还会再次限制直接来源必须是 `172.30.0.40/32`。

不要设置 `noTLSVerify`：Tunnel 到 Caddy 的链路已经使用隔离 Docker 网络中的 HTTP，外部浏览器到
Cloudflare 仍是标准 HTTPS。

Tunnel 与 Access 的官方原理和 Token 验证要求：

- <https://developers.cloudflare.com/cloudflare-one/setup/secure-private-apps/private-web-app/>
- <https://developers.cloudflare.com/cloudflare-one/access-controls/applications/http-apps/self-hosted-public-app/>

## 9. 检查并启动

本项目提供 `docker-compose.cloudflare.yml` 和 `Caddyfile.cloudflare`。每次部署都同时加载基础 Compose
与 Cloudflare 覆盖文件：

```bash
cd /opt/SaltedBlog/deploy

sudo docker compose \
  -f docker-compose.yml \
  -f docker-compose.cloudflare.yml \
  config --quiet

sudo docker compose \
  -f docker-compose.yml \
  -f docker-compose.cloudflare.yml \
  pull

sudo docker compose \
  -f docker-compose.yml \
  -f docker-compose.cloudflare.yml \
  up -d --no-build

sudo docker compose \
  -f docker-compose.yml \
  -f docker-compose.cloudflare.yml \
  ps
```

首次启动流程：PostgreSQL 健康检查 → 最小权限数据库角色初始化 → 独立迁移任务 → 创建初始管理员 →
API/Web/Caddy/cloudflared 启动。

检查关键日志：

```bash
sudo docker compose -f docker-compose.yml -f docker-compose.cloudflare.yml logs --tail=100 db-init migrate
sudo docker compose -f docker-compose.yml -f docker-compose.cloudflare.yml logs --tail=100 api web caddy cloudflared
```

所有服务应为 `running`/`healthy`，`migrate` 和 `db-init` 应以状态码 0 正常退出。

## 10. 上线验证

### 公开站点

```bash
curl -I https://blog.example.com/
curl -I https://blog.example.com/admin
```

首页应返回 200；公网域名的 `/admin` 应返回 404。

### 管理域名

在一台未登录 Cloudflare Access 的浏览器中打开：

```text
https://admin.example.com/admin
```

正确顺序是：

1. Cloudflare Access 身份验证；
2. SaltedBlog 管理员登录；
3. 首次登录绑定 TOTP；
4. 离线保存 10 个恢复码；
5. 进入管理仪表盘。

使用未获授权的邮箱应停留在 Cloudflare 拒绝页面，不能看到 SaltedBlog 登录页。

### 绕过检查

确认 `admin.example.com` 没有指向源站 IP 的独立 A/AAAA 记录。在服务器外执行：

```bash
curl -I --resolve admin.example.com:80:SERVER_PUBLIC_IP \
  http://admin.example.com/admin
```

应返回 404。它证明即便有人知道源站 IP，也不能通过 Caddy 的内部管理虚拟主机绕过 Tunnel。

## 11. 日常更新

```bash
cd /opt/SaltedBlog
git fetch --all --tags
git checkout <新的已验证-tag-或-commit>
cd deploy
# 修改 deploy/.env 中的 SALTEDBLOG_IMAGE_TAG，使其与已发布的 v* 标签一致
sudo docker compose -f docker-compose.yml -f docker-compose.cloudflare.yml pull
sudo docker compose -f docker-compose.yml -f docker-compose.cloudflare.yml up -d --no-build
sudo docker compose -f docker-compose.yml -f docker-compose.cloudflare.yml ps
```

更新前先在后台 `/admin/backup` 生成并下载一份备份。升级后检查迁移日志、公开首页、后台登录和定时任务。
回滚时把 `SALTEDBLOG_IMAGE_TAG` 改回上一版本或 `sha-<完整提交 SHA>`，再次执行 pull/up。

为了避免每次重复 `-f`，可以创建仅用于当前 shell 的别名：

```bash
alias sb-compose='sudo docker compose -f docker-compose.yml -f docker-compose.cloudflare.yml'
```

## 12. 备份、恢复与密钥保管

- PostgreSQL 数据保存在 `pg_data` named volume；上传和应用备份保存在 `app_data`。
- 后台备份包含数据库 dump 与 uploads，并使用 `backup_signing_key` 签名。
- 至少保留一份服务器外备份，并定期验证能够恢复。
- 不要只备份 Docker volume；同时离线保存 `mfa_encryption_key`、`backup_signing_key` 和数据库密码。
- 恢复后不要更换 `mfa_encryption_key`，否则现有管理员 TOTP 无法解密。

如果丢失 TOTP 设备且恢复码不可用，可在服务器重置管理员 MFA：

```bash
cd /opt/SaltedBlog/deploy
sudo docker compose -f docker-compose.yml -f docker-compose.cloudflare.yml \
  run --rm migrate salted-api reset-mfa admin
```

## 13. 常见故障

### Access 登录后出现 502

检查 cloudflared 是否在线、Published route 是否指向 `http://caddy:8080`，以及 HTTP Host Header 是否为
`admin.example.com`：

```bash
sb-compose logs --tail=200 cloudflared caddy
```

### Access 登录后出现 404

确认：

- `.env` 中 `ADMIN_DOMAIN` 与 Tunnel hostname 完全一致；
- `ADMIN_ALLOWED_CIDRS=172.30.0.40/32`；
- cloudflared 在 `edge` 网络中的固定地址仍为 `172.30.0.40`；
- 启动时确实加载了 `docker-compose.cloudflare.yml`。

### 登录接口提示 Origin/CSRF 错误

确认浏览器地址和 `ADMIN_ORIGIN` 都是 `https://admin.example.com`，不要混用 IP、HTTP 或其他别名。

### 管理域名直接显示 SaltedBlog 登录页，没有 Access 页面

立即停止 Tunnel 路由，检查 Access 应用是否覆盖整个 `admin.example.com`，并开启 Protect with Access。
Cloudflare 建议先建立 Access 应用再发布 hostname，以避免意外裸露。

### Caddy 无法为公开域名签发证书

确认 `blog.example.com` 的 A/AAAA 指向服务器、80/443 可达、没有错误的 AAAA 记录，并检查：

```bash
sb-compose logs --tail=200 caddy
```

## 14. 最终安全检查表

- [ ] 公网域名 `/admin`、`/api/admin/*`、`/api/auth/*` 返回 404；
- [ ] 管理域名没有直连源站的 A/AAAA 记录；
- [ ] Access 策略只允许具体邮箱，没有 Everyone/Bypass；
- [ ] Tunnel 开启 Protect with Access；
- [ ] cloudflared Token 只存在于权限为 600 的 Secret 文件；
- [ ] SaltedBlog 强制 TOTP，并已离线保存恢复码；
- [ ] 数据库/API/Web 未映射宿主机端口；
- [ ] 管理员密码、MFA 密钥、备份签名密钥均不复用；
- [ ] 已生成并下载第一份站外备份；
- [ ] SSH 禁止密码登录，系统与 Docker 定期更新。

## 参考资料

- Docker Engine on Ubuntu：<https://docs.docker.com/engine/install/ubuntu/>
- Cloudflare 私有 Web 应用：<https://developers.cloudflare.com/cloudflare-one/setup/secure-private-apps/private-web-app/>
- Cloudflare Access 自托管应用：<https://developers.cloudflare.com/cloudflare-one/access-controls/applications/http-apps/self-hosted-public-app/>
- Cloudflare Access 策略：<https://developers.cloudflare.com/cloudflare-one/access-controls/policies/>
- Cloudflare Tunnel 参数：<https://developers.cloudflare.com/tunnel/configuration/>
