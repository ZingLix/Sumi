# 变更记录

## Unreleased

- 支持公开 Base URL、非默认端口、子路径 MCP 与 OAuth 发现，兼容原 SUMI_RESOURCE_URL；增加 Docker Compose、反代和更新指南，以及容器 SIGTERM 退出支持。
- 实现 16 个个人账本 MCP 工具，覆盖账户、账单证据、收支、转账、退款、分类、汇率、对账、导出与恢复。
- 使用整数金额、原币复式平衡、不可变分录和冲销纠错，支持幂等提交、版本检查与个人账本隔离。
- 增加领域、身份、数据库及 HTTP MCP 模拟账单导入验收，包括个人导出和 PostgreSQL 备份恢复。
- 准备 GitHub CI、Rust 1.88 兼容检查、提交范围检查、依赖更新配置和协作说明。
