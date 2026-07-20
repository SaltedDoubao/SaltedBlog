# 发布流程

任何版本升级、镜像发布、创建或推送 `v*` 标签、修改生产 Dockerfile 或请求发布 GitHub Release 的任务，必须先使用仓库内的 `release-preflight` 技能：

1. 阅读 `.codex/skills/release-preflight/SKILL.md` 并执行其自检脚本。
2. 在目标提交推送到 `main` 后，确认同一提交的 `release-preflight` GitHub Actions 工作流成功。
3. 仅在上述检查通过后，才创建并推送注释发布标签。
4. 若标签构建失败，禁止移动、删除或强制重推该标签；修复后升级补丁版本并重新走完整流程。
