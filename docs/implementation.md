# 实现与运行

设计中的第一版功能已实现。验收程序在 tests/postgres.rs、tests/mvp/mod.rs 和 tests/mvp/edges.rs；纯领域和身份检查在 tests/domain.rs、tests/auth.rs。本项目不内置邮箱、OCR、模型、调度或前端。

## 服务与权限

需要 Rust 1.88+、PostgreSQL 16+。依赖由 Cargo.lock 固定，rmcp 及宏同步锁定为 1.7.0。程序采用无会话 Streamable HTTP，每次请求重新校验 OAuth 身份，不从工具参数取得操作者。

数据库管理员先创建 sumi 数据库，以及独立的 sumi_app LOGIN 角色并设置密码。迁移账户拥有表；HTTP 运行账户不能拥有或继承表所有者角色，不能拥有 SUPERUSER、BYPASSRLS 权限。所有业务表强制启用 RLS，账户、来源、观测、证据、汇率与操作均通过带 ledger_id 的外键关联。服务启动检查运行角色及迁移完整性。

~~~sh
export SUMI_ADMIN_DATABASE_URL='postgres://migration_owner:password@127.0.0.1:5432/sumi'
DATABASE_URL="$SUMI_ADMIN_DATABASE_URL" cargo run --locked -- migrate
psql "$SUMI_ADMIN_DATABASE_URL" -v ON_ERROR_STOP=1 -f scripts/grant-runtime.sql

# 将 .env.example 复制为 .env 并填写实际配置
set -a
source .env
set +a
cargo run --locked
~~~

程序从进程环境读取配置，不自动加载 .env。默认监听 127.0.0.1:8080。SUMI_BASE_URL 配置公开访问地址，可带子路径；例如 https://example.com/sumi 对应 MCP 路径 /sumi/mcp。旧 SUMI_RESOURCE_URL 完整地址仍兼容，同时设置时必须一致。/healthz 和相应子路径 /sumi/healthz 用于进程存活检查。远程部署由反向代理终止 HTTPS，保留完整路径、原始 Host 和 Authorization，并支持 Streamable HTTP。服务校验 Host 和存在时的 Origin，不依赖 X-Forwarded-* 确定身份。整体请求体上限 1 MiB，单份归档正文上限 512 KiB，来源及观测批量最多 100 项，交易最多 50 动作，列表默认 50、最多 100 项。业务版本变化使旧游标报 CURSOR_STALE。容器、子路径反代、更新及备份步骤见 [自部署](self-hosting.md)。

## OAuth

授权服务器由部署方提供，签名公钥从 SUMI_OAUTH_JWKS_FILE 载入。资源服务器不签发令牌，不提供身份绕过模式。公钥文件在启动时读取，轮换后重启服务。

- Header：typ=at+jwt，alg 为 RS256 或 ES256，kid 对应允许验签的公钥。
- Claims：iss 与 SUMI_OAUTH_ISSUER 一致，aud 包含由 SUMI_BASE_URL 推导的完整 MCP URL（或显式 SUMI_RESOURCE_URL），具有有效的 sub、client_id、iat、exp 和 scope。存在 nbf 时同样校验，时钟容差 30 秒。
- 读取需要 sumi:read，写入需要 sumi:write，通常同时申请两项。资源 URL 必须是客户端访问的完整 HTTPS /mcp URL；本机开发允许 loopback HTTP。
- 首次 context 初始化个人账本及该客户端授权。实际权限为 JWT scope、当前用户所有权与 authorization_grants 中授权的交集。运维将授权 status 改为 revoked 或将用户 status 改为 disabled 后，后续请求即被拒绝；普通调用不能重新开启已撤销授权。

资源元数据位于 /.well-known/oauth-protected-resource 加完整 MCP 路径，例如 /.well-known/oauth-protected-resource/sumi/mcp（另有根路径别名），401 返回对应 WWW-Authenticate challenge。测试目录中的密钥仅用于自动化测试，部署使用授权服务器自己的公钥。

## 16 个工具

| 工具 | 行为 |
| --- | --- |
| sumi_context_get | 初始化或读取个人账本、账户、偏好、分类、币种及未处理事项 |
| sumi_accounts_list / upsert | 原币余额、已确认/未知期初，实际账户和币种子账户管理 |
| sumi_sources_ingest / get | 文本或 JSON 归档、服务器哈希、不可变版本和定位 |
| sumi_observations_ingest / search | 稳定行身份、提取版本、处理备注、忽略理由、导入批次和证据占用 |
| sumi_fx_rates_ingest / query | 有来源的不可变日期报价，方向为 1 from_currency = rate to_currency |
| sumi_transactions_apply | 原子新增、更正、作废、关联/解除证据和期初，支持预览 |
| sumi_transactions_search / get | 当前交易过滤、重复候选、完整版本与凭证、证据历史和备注 |
| sumi_reconcile | 指定账单范围的原币余额和资金行双向核对，独立返回导入完整性 |
| sumi_reports_get | 收支、分类、账户余额、数据质量和完整个人导出 |
| sumi_operations_get | 按 operation_id 或当前客户端的 tool + idempotency_key 恢复提交结果 |
| sumi_preferences_update | 版本化分类、标签、商户别名、分类规则和展示设置 |

工具 JSON Schema 是实际参数定义。交易使用 action.transaction.movement 结构。日期和实际金额来自调用方资料，未知发生日不补造，可保存 occurred_period（YYYY 或 YYYY-MM）表示已知精度；occurred_at 存在时使用账本时区验证日期。银行可用余额、预授权和未付款订单只作为观测，不自动记支出。

金额始终是最小单位整数字符串。Rust 使用受限 i128 和 checked 运算，SQLx 仅在数据库边界使用 BigDecimal。数据库 NUMERIC domain 检查整数及 38 位范围，避免 NUMERIC(38,0) 在检查前自动舍入。支持币种及 JPY/KWD 等精度由版本化的 sumi-currencies-1 表返回。

## 交易与纠错

先调用 sumi_context_get({})，取得 ledger_id，创建 asset 或 liability 实际账户。信用卡欠款采用负余额。期初通过 set_opening 保存，as_of 为跟踪起点前一日的收盘日期；零期初是已确认基准，未知期初不会默认为零。

~~~json
{
  "ledger_id": "<账本 UUID>",
  "idempotency_key": "purchase-1",
  "actions": [{
    "op": "create",
    "transaction": {
      "occurred_on": "2026-10-15",
      "description": "消费",
      "merchant": "商户原始名称",
      "movement": {
        "kind": "expense",
        "payment_account_id": "<账户 UUID>",
        "money": {"currency": "CNY", "amount_minor": "10000"},
        "posted_on": "2026-10-15"
      },
      "allocations": [
        {"category_id": "<分类 UUID>", "amount_minor": "7000"},
        {"category_id": null, "amount_minor": "3000"}
      ],
      "evidence_refs": [{"observation_id": "<观测 UUID>", "version": 1}]
    }
  }]
}
~~~

服务返回稳定 allocation_id。修正时沿用相应 ID，填写 transaction_id、expected_version、reason 和完整 transaction。真实退款逐项引用原消费的 allocation_id，原币本金与实际到账分别保存，累计金额同时受原消费和各项限制；原消费重分类后退款统计读取当前分类。涉及原消费与退款的修改按整批最终状态校验。

转账和信用卡还款本金不进入收支。费用放在 transaction.fees 中，各自具有账户、金额、日期、分类与可选实际结算依据。跨币种本金通过各币种交换科目平衡，跨日转账经在途科目，未完成转账使用 unresolved_movement 和 transfer_in_transit；到账后更正同一交易。

资料更正不生成分录，财务更正完整冲销当前有效凭证，再追加正确凭证；冲销保留旧 effective_on，不使用发现错误的日期。停用账户保留历史并允许更正旧记录，不能新增无关资金业务。正式分录和业务历史不可更新、删除，已提交凭证不能再追加分录。

成功请求原键重试返回原结果。新键先检查版本，相同业务返回 changed=false，不增加实体版本、分录和 ledger_revision；请求理由本身不构成业务变更。强交易标识在账本、命名空间、金融账户、事件类型和外部 ID 范围内永久唯一，作废也不释放。相同标识与相同资金事实复用交易并追加证据，资金事实不同则冲突。同日同商户同金额只返回候选，保留独立消费及已有解释。

preview=true 在事务内执行相同规则和数据库约束，最后回滚，返回 changed=false、would_change 和 proposed_revision。预览 ID 是临时值，实际提交使用新幂等键。每天、每个币种的凭证均独立平衡。

## 来源、观测与复核

来源身份为 provider + source_scope + external_id，正文变化形成不可变版本。capture_kind 为提交方声明的 original、excerpt、ai_normalized 或 user_statement；哈希证明已保存内容未变，不认证外部来源真实。

相同来源版本、record_locator 和观测类型复用稳定观察 ID。材料换版时必须明确映射旧 observation_id；身份不明返回 SOURCE_CONFLICT，并通过上下文的 unmapped_sources 提示待处理来源。确属新行时显式传 new_record_confirmed 及理由，不能用换版自动制造另一份资金观测。

Observation.version 用于所有更新，current_version 仅指提取载荷版本。只改 review_note 或 ignored_reason 保留提取版本及证据。真实资金行不能被忽略，提取载荷改版清除原忽略理由。检索可按 observation_id 并 include_history 读取备注及提取历史。

新版观测和旧证据的待复核状态在同一事务生效，保留旧账和旧金额占用。金额变小，或账户、币种、方向改变，都能保存真实更正并返回冲突。存在旧占用待复核时不能新增分配；通过一次 apply 更正相关交易、关闭旧关联并重绑新版证据，最终状态重新检查账户、币种、方向、日期和金额。只解除证据也保留账目，并明确返回 evidence_missing。

不同来源分别校验，同一稳定观测所有版本共享占用，关闭的历史关联不重复占用。资金证据必须是完整 movement，order、authorization 等只能作为 context 资料；统计不会因证据过期隐藏旧收支。导入批次保存来源范围、完成状态和版本，实际资金行条数及收付总额由服务计算，它只是分组，不替代去重或跨调用事务。

## 对账与完整性

账单 statement 指定账户、币种、[period_start,period_end)、原币期初期末、来源覆盖及提取范围。应有条数、付款总额、收款总额均可选，缺少时返回 unavailable，有值时必须检查。普通重导不会漏传清除已知控制值；误提取的控制可通过 clear_controls、control_correction_locator 及理由明确纠正，保留历史。

reconciliation 独立判断余额和已取得流水的双向足额匹配，返回 reconciled、balance_matched、mismatch 或 incomplete。只有余额不能以空集合匹配宣称成功，具备明确零流水依据的账单可以。未知期初、必要日期不足或推断日期不能确认跨期对账。

import_completeness 独立返回 complete、incomplete 或 unknown。完整范围依据可来自结构化原文中可核对的完整记录集合，也可来自已验证定位的控制值与完整来源范围。支持 JSON Pointer、line:N、$（整份原文）定位，无法核对的依据不会自动证明完整。结构化记录范围可以是数组字符串，或带 locator 字段的记录数组，statement.record_locators 必须与原文和实际资金行集合一致。

已知部分材料、尚未完成的批次、可用控制不符、记录范围缺口都会返回 incomplete；仅缺少可选控制值不会阻止对账。原文取得完整、AI 声明完成或余额相等本身都不证明所有行已提取。reconciled 可与 unknown/incomplete 并存，完整导入也可与余额差异并存。结果保存范围和 ledger_revision，后续业务变化使两个旧结论同时标为 stale。

## 统计与导出

收支读取当前业务版本，按 occurred_on 返回收入、原始支出、真实退款、净支出和未分类金额。手续费独立计入费用，纠错冲销不算退款。账户余额按实际入账日累计所有正式分录；未知发生日进入未归期列表。支持账户、币种、商户、分类、标签及日期过滤。

展示币种优先使用有来源的实际结算，退款可使用已知实际到账，否则用发生日的不可变报价。余额估算使用所选时点报价。缺报价不阻塞原币入账，结果列出未折算组成项及依据。报价计算使用任意精度有理数，总额 half_even 舍入，再按稳定 allocation_id 做最大余数分配，之后才筛选分类和标签；退款先按正金额分配再从净支出扣减。

reports_get(report=export) 在单个 PostgreSQL repeatable-read 快照内导出个人全部业务、证据、版本、分录、授权、备注、汇率、对账结果及操作历史，金额仍为字符串，包含格式版本和内容哈希。恢复仅通过管理命令执行，不开放给 MCP：

~~~sh
DATABASE_URL="$SUMI_ADMIN_DATABASE_URL" cargo run --locked -- restore /path/to/export.json
~~~

目标应先完成迁移，且不能已有该用户或账本。恢复验证哈希、所有权和账务约束，一次事务完成，不会覆盖已有账本。JSON 文件是 export 响应 data 字段。

整库一致性备份使用管理连接和 PostgreSQL 客户端。备份先写临时文件，成功后才发布，且不覆盖已有目标；SUMI_PG_DUMP 可指定客户端路径：

~~~sh
DATABASE_URL="$SUMI_ADMIN_DATABASE_URL" sh scripts/backup.sh /path/to/new-backup.dump
# 恢复到新建数据库，恢复后重新授予运行角色权限
pg_restore --no-owner --no-acl --exit-on-error --dbname="$SUMI_RESTORE_DATABASE_URL" /path/to/new-backup.dump
psql "$SUMI_RESTORE_DATABASE_URL" -v ON_ERROR_STOP=1 -f scripts/grant-runtime.sql
~~~

## 验证

~~~sh
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked

SUMI_TEST_ADMIN_URL='postgres://postgres@127.0.0.1:5432/postgres' \
SUMI_TEST_PG_BIN='/path/to/postgresql-16/bin' \
  cargo test --test postgres --locked -- --ignored
~~~

集成测试只允许本机数据库，创建随机独立数据库和运行角色。个人 JSON 恢复和 pg_dump/pg_restore 也使用新建数据库，成功后清理测试数据库；测试失败时可保留临时库排查。未配置 SUMI_TEST_PG_BIN 时只跳过 PostgreSQL 客户端备份子场景，CI 配置会提供该目录并执行完整恢复检查。

tests/mvp/mcp_import.rs 另用临时 loopback HTTP 服务和 curl 客户端验证完整 MCP 导入流程。原始模拟中文账单位于 tests/fixtures/statement.csv，包含工资、两笔消费、部分退款和本人账户转账。客户端使用 AI 按原文整理的固定结构化事实，完成工具发现、来源归档和读取、观测及批次保存、预览、证据关联入账、重复导入、收支查询、余额核对及对账；不直接调用 Store 业务方法。此场景验证协议和账务链路，不运行模型，也不代表真实银行格式、PDF/OCR 或模型提取准确率已验收。使用它需要本机安装 curl。
