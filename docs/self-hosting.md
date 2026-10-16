# 自部署

Sumi 可通过二进制或 Docker Compose 自部署。服务监听地址与公网访问地址独立配置；公网域名、非默认端口和子路径均可使用。服务本身通过 HTTP 连接反向代理，由代理提供 HTTPS。

## 地址配置

| 配置 | 用途 | 示例 |
| --- | --- | --- |
| SUMI_BIND | 进程实际监听地址 | 127.0.0.1:8080；容器内为 0.0.0.0:8080 |
| SUMI_BASE_URL | 用户访问的公开地址，不含 /mcp | https://ledger.example.com 或 https://example.com/sumi |
| SUMI_RESOURCE_URL | 兼容旧配置的完整 MCP 地址 | https://example.com/sumi/mcp |
| SUMI_OAUTH_ISSUER | 外部授权服务器的 issuer，必须与令牌 iss 完全一致 | https://auth.example.com/ |
| SUMI_OAUTH_JWKS_FILE | 服务端读取的授权服务器公钥文件 | /etc/sumi/public-jwks.json |

推荐只设置 SUMI_BASE_URL。同时设置 SUMI_RESOURCE_URL 时，两者必须指向同一个 MCP 地址，否则启动报错。BASE_URL 的末尾斜杠会移除；子路径使用 ASCII 字母、数字、-、_、.、~。不能携带账号密码、查询参数或 fragment。远程地址要求 HTTPS；loopback 地址允许 HTTP。

例如 SUMI_BASE_URL=https://example.com:8443/apps/sumi：

- MCP：`https://example.com:8443/apps/sumi/mcp`
- OAuth resource / JWT aud：`https://example.com:8443/apps/sumi/mcp`
- 资源发现：`https://example.com:8443/.well-known/oauth-protected-resource/apps/sumi/mcp`
- 存活检查：`https://example.com:8443/apps/sumi/healthz`

资源发现地址按 [RFC 9728](https://www.rfc-editor.org/rfc/rfc9728.html#section-3.1) 位于域名根目录的 /.well-known 下，因此子路径代理也必须转发这个独立位置。/healthz 始终可供本机和容器存活检查使用。修改公网地址后，需要让客户端使用新地址，并让授权服务器签发以新完整 MCP 地址为 audience 的访问令牌；用户身份由 issuer + subject 决定，更换 issuer 需要单独规划身份迁移。

## Docker Compose

需要 Docker Engine 与 Compose 插件。仓库的 Dockerfile 编译 Rust 服务，以非 root 用户运行；构建上下文仅允许源码、迁移、Cargo 文件和许可证，真实邮件及 .local/ 不进入镜像。

1. 复制 deploy/compose.env.example 到 .local/compose.env，分别用 openssl rand -hex 32 生成两个不同数据库密码并填入。示例用十六进制密码，可直接放进连接 URI；自行使用其他字符时须正确 URL 编码。
2. 设置公网 SUMI_BASE_URL 和授权服务器 SUMI_OAUTH_ISSUER，将该授权服务器的公开 JWKS 保存到 SUMI_OAUTH_JWKS_HOST_FILE。路径以仓库根目录为准，容器 UID 10001 必须能读取该文件。仅公开公钥需要挂载，签名私钥由授权服务器管理。
3. 启动服务：

~~~sh
docker compose --env-file .local/compose.env up -d --build sumi
docker compose --env-file .local/compose.env logs --tail=100 sumi
curl --fail http://127.0.0.1:8080/healthz
~~~

首次启动顺序是 PostgreSQL 初始化（含运行角色）→ 数据库 TCP 健康检查 → 迁移 → 授权 → 启动服务。数据库持久化到 db_data 命名卷，不映射宿主机端口；服务端口默认仅绑定宿主机 127.0.0.1:8080，供同机反向代理访问。SUMI_PORT 可修改宿主机端口。

数据库初始化脚本只在空数据卷执行。修改环境文件中的密码不会修改现有数据库角色密码；更换密码须在数据库中修改对应角色，再更新运行配置。已有外部 PostgreSQL 时，可直接运行二进制或使用 Dockerfile 镜像并自行提供 DATABASE_URL，迁移账户与运行账户仍须分开。

更新代码时，先完成下述备份，再执行：

~~~sh
docker compose --env-file .local/compose.env stop sumi
docker compose --env-file .local/compose.env build sumi migrate
docker compose --env-file .local/compose.env run --rm migrate
docker compose --env-file .local/compose.env run --rm permissions
docker compose --env-file .local/compose.env up -d --no-deps --force-recreate sumi
~~~

迁移失败时保留停止状态，检查日志及迁移说明后再启动。普通停止使用 docker compose stop，移除容器使用 docker compose down；数据卷独立保留。删除卷的 --volumes 选项会删除数据库，不用于正常更新。服务响应 SIGTERM 和 SIGINT 并等待正在处理的 HTTP 请求退出。

## 反向代理

独立域名可参考 [Caddy 示例](../deploy/Caddyfile.example)，由 Caddy 获取和维护 HTTPS 证书。已有 HTTPS Nginx 站点可将 [location 示例](../deploy/nginx.conf.example) 放入 server 块。两者都要保留完整路径、外部 Host（包括非默认端口）和 Authorization，并关闭响应缓冲。

代理 upstream 使用内部 HTTP。服务按配置的外部 Host、Origin 和 JWT audience 校验，不根据客户端提交的 X-Forwarded-Host 或 X-Forwarded-Proto 放宽检查。代理不能剥掉 /sumi 之类的前缀；使用子路径时，还须添加相应的 /.well-known 精确路由。

~~~sh
# 将示例域名和路径换成你的配置
curl --fail https://example.com/sumi/healthz
curl --fail https://example.com/.well-known/oauth-protected-resource/sumi/mcp
curl --include --request POST https://example.com/sumi/mcp
~~~

最后一条未带令牌，应返回 401，并在 WWW-Authenticate 中给出完整公网发现地址。传入访问令牌后可使用支持 Streamable HTTP 的 MCP 客户端连接公开 MCP URL。

## 身份验证

Compose 提供账本服务与数据库，授权服务器由部署方提供。Sumi 校验 OAuth access token，当前不会生成登录页、注册用户口令或签发客户端令牌。需要配置授权服务器对该 MCP resource 的 audience、sumi:read / sumi:write scope、client_id 和 JWT 签名参数。具体令牌字段见 [OAuth 配置](implementation.md#oauth)。

部署用授权服务器自己的公钥，不能使用 tests/fixtures 中公开的测试签名密钥。JWKS 启动时载入，公钥轮换后重新创建服务容器；保持旧公钥到存量有效令牌过期可避免过渡期中断。

## 备份与恢复

定期保存数据库备份到宿主机或独立存储。下例使用 PostgreSQL 16 容器内客户端，密码来自容器自身配置，备份目录不进入 Git。每次使用新的文件名：

~~~sh
mkdir -p .local/backups
backup_path=".local/backups/sumi-$(date -u +%Y%m%dT%H%M%SZ).dump"
(
  umask 077
  set -C
  docker compose --env-file .local/compose.env exec -T db \
    sh -ec 'export PGPASSWORD="$POSTGRES_PASSWORD"; exec pg_dump -U "$POSTGRES_USER" -d "$POSTGRES_DB" --format=custom' \
    > "$backup_path"
)
~~~

命令失败时不要使用生成的文件作为有效备份。也可使用 [原子发布备份脚本](../scripts/backup.sh) 和管理连接。正式启用前先验证新库恢复，恢复后重新执行 scripts/grant-runtime.sql；个人 JSON 导出和恢复见 [实现与运行](implementation.md#统计与导出)。

CI 的 container 任务会使用合成身份公钥运行完整 Compose 启动，检查子路径存活探针、401 challenge、资源发现、签名 MCP 读写和幂等提交，并在保留数据卷的情况下重启整套服务，确认余额和收支不变；数据库验收另外检查子路径上的完整 HTTP MCP 账单导入、个人导出及 pg_dump/pg_restore。
