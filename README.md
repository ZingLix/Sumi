# Sumi

Sumi 是供 Codex 等 AI 操作的个人日常记账服务端。AI 获取邮件和账单、理解交易，再通过 MCP 导入；服务端保存可信账目、证据和修改历史，完成去重、对账和基础统计。

第一版保留服务端、多用户、多币种、复式记账。多用户按每人独立账本设计，日常操作围绕支出、收入、转账、信用卡还款、退款和手续费。技术方案为 Rust、PostgreSQL、HTTPS Streamable HTTP MCP。

“可信”指来源可追溯、账务约束由服务端强制、修改可解释、不确定性可见。采用成熟账本内核和 AI 导入、纠错接口：已入账、证据支持、对账结果和导入完整性分别表达。第一版不独立认证外部材料真伪，不要求日常操作逐笔审批。

| 文档 | 内容 |
| --- | --- |
| [整体架构](docs/architecture.md) | 系统边界、用户隔离、AI 工作流程与可靠写入 |
| [数据模型](docs/data-model.md) | 个人账户、收支、分类标签、来源、不可变分录和历史 |
| [多币种记账](docs/currency-accounting.md) | 原币复式平衡、换汇与还款、个人收支折算 |
| [MCP 接口](docs/mcp-api.md) | 16 个工具、批量写入、检索、纠错和对账 |
| [第一版范围](docs/mvp.md) | 必做功能、不纳入的复杂能力及验收场景 |

设计中的 MVP 已实现：16 个 MCP 工具覆盖账户、来源与观测、跨版本证据占用及复核、强标识去重、收支与转账、逐项退款、独立手续费、分类标签、精确折算、原币双向对账、独立导入完整性、操作恢复及完整导出。资金更正追加冲销，资料更正保留原凭证，相同业务不重复改版。

PostgreSQL 通过组合外键、强制 RLS、延迟平衡检查和不可变历史保护账目。JWT scope 与保存的个人账本授权取交集，支持运维撤销授权和停用用户。posted、证据支持、对账和导入完整性分别返回，不把资料不足解释为成功。

- [启动、接口示例与实现进度](docs/implementation.md)
- [开发、提交范围与 GitHub 首次发布](CONTRIBUTING.md)
- [变更记录](CHANGELOG.md) · [MIT 许可证](LICENSE)
- 运行本地检查：`cargo fmt --check`、`cargo clippy --all-targets --locked -- -D warnings`、`cargo test --locked`
- 数据库验收：配置本机 `SUMI_TEST_ADMIN_URL` 和包含 pg_dump/pg_restore 的 `SUMI_TEST_PG_BIN` 后运行 `cargo test --test postgres --locked -- --ignored`。测试创建独立数据库和运行角色，覆盖设计场景、个人导出恢复及 PostgreSQL 备份恢复，成功后删除测试数据库。
