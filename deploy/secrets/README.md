# 生产 Secrets

此目录中的真实密钥已被 Git 忽略。首次部署前创建下列文件，每个文件仅包含一行值：

- `postgres_superuser_password`
- `postgres_owner_password`
- `postgres_app_password`
- `database_url`：`postgres://salted_app:<APP密码>@postgres:5432/saltedblog`
- `database_maintenance_url`：`postgres://salted_owner:<OWNER密码>@postgres:5432/saltedblog`
- `admin_password`：首次创建管理员使用，至少 15 个字符
- `mfa_encryption_key`、`backup_signing_key`：分别生成的 32 字节随机值
- `news_llm_api_key`：不使用 LLM 时创建空文件

Linux/macOS 可用 `openssl rand -base64 32` 生成密码或密钥。设置目录权限：

```bash
chmod 700 deploy/secrets
chmod 600 deploy/secrets/*
```

不要复用任意两个密钥。丢失 `mfa_encryption_key` 会导致现有 TOTP 无法解密；丢失
`backup_signing_key` 会导致现有 v2 备份无法通过网页恢复，因此应将二者离线保存。
