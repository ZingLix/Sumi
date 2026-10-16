# 开发与协作

开发检查需要 Rust 1.88+、PostgreSQL 16+、curl 和 Python 3。启动、权限配置和完整验收命令见 [实现与运行](docs/implementation.md)。提交 Cargo.lock；更新依赖后使用 --locked 验证，rmcp 与 rmcp-macros 的锁定版本须一致。

## 每次修改

从 main 创建功能分支，通过 Pull Request 合并。PR 说明用户可见的变化、验证结果及数据迁移影响；同步更新设计或接口文档和 CHANGELOG.md 的 Unreleased 部分。

~~~sh
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
cargo +1.88 check --all-targets --locked
~~~

修改交易、金额、证据、权限、数据库结构或备份恢复时，再执行数据库验收：

~~~sh
SUMI_TEST_ADMIN_URL='postgres://postgres@127.0.0.1:5432/postgres' \
SUMI_TEST_PG_BIN='/path/to/postgresql-16/bin' \
  cargo test --test postgres --locked -- --ignored --nocapture
~~~

验收使用本机可丢弃的 PostgreSQL 集群，创建独立数据库与角色，不连接实际使用的账本。CI 会运行上述检查、真实 HTTP MCP 模拟账单导入和 pg_dump/pg_restore 恢复场景。CI 首次在 GitHub 运行成功后，再将 Rust checks 工作流中的 check 与 msrv 任务设为 main 的合并检查。

## 数据与接口演进

- 金额始终使用最小单位整数字符串，不经过浮点计算。
- 已提交分录与历史不可修改；财务纠错追加冲销，资料修改保留原分录。
- 账户、证据和授权必须保持账本隔离。更新观测不静默覆盖已入账事实。
- 新增数据库迁移文件，不修改已经发布的迁移；迁移后检查运行角色授权和 RLS。修改持久化结构时验证旧个人导出文件的兼容性及恢复路径。
- 破坏性 API、导出格式或配置变更须在 PR 和变更记录中给出迁移方法。发布使用 v0.x.y 标签，版本与 Cargo.toml 保持一致。

## 提交范围

真实邮件、账单、导出、数据库备份、运行密钥和本地导入脚本放在 .local/，不进入版本库。提交的测试资料使用合成数据；问题报告与日志同样先去除个人信息和令牌。

tests/fixtures/test-key.pem 是公开的、无生产用途的自动化测试密钥，匹配 tests/fixtures/jwks.json。仓库检查仅允许这份已知测试密钥的精确内容。不能用于正式服务。

暂存后、提交或首次推送前运行：

~~~sh
git add .
python3 scripts/check-repository.py
git diff --cached --stat
~~~

初始化或克隆仓库后，可启用仓库自带的 pre-commit 检查，让上述提交范围检查在每次提交前运行：

~~~sh
git config core.hooksPath .githooks
~~~

检查脚本读取 Git 暂存区，拦截本地数据路径、备份、未批准的私钥、JWT 和非示例数据库密码；修改忽略规则不会移除已经跟踪的文件，因此强制暂存的本地资料也会被检查。GitHub CI 运行相同检查。GitHub 仓库启用 secret scanning / push protection 后，还可对推送中的受支持密钥增加平台检查。

## 首次发布

1. 创建 GitHub 空仓库，选择公开或私有；本地 git init -b main，然后启用上述提交钩子。
2. 暂存文件，运行仓库检查并查看暂存差异，随后创建首次提交并配置实际 remote。
3. 推送 main，确认 CI 成功；在仓库设置启用 main 分支保护、Dependabot 安全更新和可用的 secret scanning / push protection。

GitHub 迭代只需要合成测试资料和 CI 自建数据库。部署真实账本时，另行配置 PostgreSQL 持久存储、独立运行角色、OAuth 授权服务器及公钥、HTTPS、备份保留与恢复验证；不把本地试用配置上传为部署配置。
