//! Agent-authored extraction of a raw statement, exercised only through HTTP MCP.
//! This is a reproducible client scenario, not a built-in CSV parser or model eval.
use super::*;
use std::process::Command;

struct Client {
    url: String,
    token: String,
}

impl Client {
    async fn rpc(&self, method: &str, params: Value) -> Value {
        let body = json!({"jsonrpc":"2.0","id":1,"method":method,"params":params}).to_string();
        let url = self.url.clone();
        let authorization = format!("Authorization: Bearer {}", self.token);
        let output = tokio::task::spawn_blocking(move || {
            Command::new("curl")
                .args([
                    "--silent",
                    "--show-error",
                    "--fail-with-body",
                    "--max-time",
                    "15",
                    "--noproxy",
                    "*",
                    "-H",
                    "Host: 127.0.0.1:8080",
                    "-H",
                    &authorization,
                    "-H",
                    "Content-Type: application/json",
                    "-H",
                    "Accept: application/json, text/event-stream",
                    "-H",
                    "MCP-Protocol-Version: 2025-11-25",
                    "--data-binary",
                    &body,
                    &url,
                ])
                .output()
                .unwrap()
        })
        .await
        .unwrap();
        assert!(
            output.status.success(),
            "HTTP MCP: {} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let response: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert!(response.get("error").is_none(), "{response}");
        response["result"].clone()
    }

    async fn call(&self, name: &str, arguments: Value) -> Value {
        let result = self
            .rpc("tools/call", json!({"name":name,"arguments":arguments}))
            .await;
        assert_ne!(result["isError"], true, "{name}: {result}");
        result["structuredContent"].clone()
    }
}

pub(super) async fn acceptance(store: &Store) {
    let resource = "http://127.0.0.1:8080/self-hosted/sumi/mcp";
    let mut claims = common::claims("http-statement-import");
    claims["aud"] = json!(resource);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client = Client {
        url: format!(
            "http://{}/self-hosted/sumi/mcp",
            listener.local_addr().unwrap()
        ),
        token: common::token(&claims),
    };
    let auth = sumi::auth::Auth::new(
        "https://auth.sumi.test/".into(),
        resource.into(),
        serde_json::from_str(include_str!("../fixtures/jwks.json")).unwrap(),
    )
    .unwrap();
    let router = server::router(store.clone(), auth);
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    client.rpc("initialize", json!({"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"statement-import-acceptance","version":"1"}})).await;
    let tools = client.rpc("tools/list", json!({})).await;
    assert_eq!(tools["tools"].as_array().unwrap().len(), 16);
    let context = client.call("sumi_context_get", json!({})).await;
    let ledger = context["data"]["ledger_id"].clone();
    let mut accounts = vec![];
    for (key, name) in [("bank", "工资卡 TEST-001"), ("savings", "储蓄卡 TEST-002")] {
        let result = client.call("sumi_accounts_upsert", json!({"ledger_id":ledger,"idempotency_key":key,"name":name,"kind":"asset","currency":"CNY"})).await;
        accounts.push(result["data"]["account"]["id"].clone());
    }
    let bank = &accounts[0];
    let savings = &accounts[1];
    let prefs = client.call("sumi_preferences_update", json!({"ledger_id":ledger,"idempotency_key":"categories","expected_version":1,"reason":"Classify the test statement","categories":[{"name":"工资","kind":"income"},{"name":"餐饮","kind":"expense"},{"name":"日用品","kind":"expense"}]})).await;
    let category = |name: &str| {
        prefs["data"]["preferences"]["categories"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["name"] == name)
            .unwrap()["id"]
            .clone()
    };
    let source = client.call("sumi_sources_ingest", json!({"ledger_id":ledger,"idempotency_key":"source","sources":[{"provider":"synthetic-bank","source_scope":"TEST-001","external_id":"2026-09","body":include_str!("../fixtures/statement.csv"),"capture_kind":"original"}]})).await;
    let source_version = source["data"]["sources"][0]["source_version_id"].clone();
    let archived = client
        .call(
            "sumi_sources_get",
            json!({"ledger_id":ledger,"source_version_id":source_version}),
        )
        .await;
    assert_eq!(
        archived["data"]["source"]["body"],
        include_str!("../fixtures/statement.csv")
    );

    // Facts extracted by the agent from the archived text. No model runs in the service.
    let rows = [
        ("2026-09-01", "TEST-SALARY", "工资", "in", "1000000"),
        ("2026-09-03", "TEST-COFFEE", "街角咖啡", "out", "3500"),
        ("2026-09-05", "TEST-MARKET", "社区超市", "out", "12800"),
        ("2026-09-06", "TEST-REFUND", "社区超市退款", "in", "2800"),
        (
            "2026-09-10",
            "TEST-TRANSFER",
            "转入本人储蓄卡",
            "out",
            "200000",
        ),
    ];
    let mut observations = rows.iter().enumerate().map(|(i, (date, _, _, direction, amount))| json!({"source_version_id":source_version,"record_locator":format!("line:{}",i+5),"extractor":"agent-authored-test","reason":"Extracted from the original synthetic statement","payload":{"type":"movement","account_id":bank,"money":{"currency":"CNY","amount_minor":amount},"direction":direction,"posted_on":date}})).collect::<Vec<_>>();
    observations.push(json!({"source_version_id":source_version,"record_locator":"$","extractor":"agent-authored-test","reason":"Statement scope and controls from archived text","payload":{"type":"statement","account_id":bank,"currency":"CNY","period_start":"2026-09-01","period_end":"2026-10-01","opening_balance":"500000","closing_balance":"1286500","expected_count":{"value":5,"locator":"line:11"},"expected_debit_total":{"value":"216300","locator":"line:12"},"expected_credit_total":{"value":"1002800","locator":"line:13"},"source_coverage":"complete","record_locators":["line:5","line:6","line:7","line:8","line:9"],"range_locator":"$","extraction_complete":true}}));
    let observed = client.call("sumi_observations_ingest", json!({"ledger_id":ledger,"idempotency_key":"extraction","observations":observations,"batch":{"name":"模拟账单 2026-09","source_version_ids":[source_version],"complete":true}})).await;
    let evidence = |i: usize| json!([{"observation_id":observed["data"]["observations"][i]["observation"]["id"],"version":1}]);
    client.call("sumi_transactions_apply", json!({"ledger_id":ledger,"idempotency_key":"opening","actions":[{"op":"set_opening","account_id":bank,"money":{"currency":"CNY","amount_minor":"500000"},"as_of":"2026-08-31","reason":"Statement opening balance"},{"op":"set_opening","account_id":savings,"money":{"currency":"CNY","amount_minor":"0"},"as_of":"2026-08-31","reason":"Explicit supplementary confirmation"}]})).await;
    let mut actions = vec![];
    for (i, cat) in ["工资", "餐饮", "日用品"].into_iter().enumerate() {
        let (date, external_id, merchant, _, amount) = rows[i];
        let kind = if i == 0 { "income" } else { "expense" };
        let account_field = if i == 0 {
            "receipt_account_id"
        } else {
            "payment_account_id"
        };
        let mut movement =
            json!({"kind":kind,"money":{"currency":"CNY","amount_minor":amount},"posted_on":date});
        movement[account_field] = bank.clone();
        actions.push(json!({"op":"create","transaction":{"occurred_on":date,"description":merchant,"merchant":merchant,"movement":movement,"allocations":[{"category_id":category(cat),"amount_minor":amount}],"identifiers":[{"namespace":"synthetic-bank","account_id":bank,"event_type":"booked","external_id":external_id}],"evidence_refs":evidence(i)}}));
    }
    let preview = client.call("sumi_transactions_apply", json!({"ledger_id":ledger,"idempotency_key":"preview","preview":true,"actions":actions})).await;
    assert_eq!(preview["changed"], false);
    assert_eq!(preview["data"]["would_change"], true);
    let first = client
        .call(
            "sumi_transactions_apply",
            json!({"ledger_id":ledger,"idempotency_key":"first-three","actions":actions}),
        )
        .await;
    let purchase_id = first["data"]["transactions"][2]["transaction_id"].clone();
    let purchase = client
        .call(
            "sumi_transactions_get",
            json!({"ledger_id":ledger,"transaction_id":purchase_id}),
        )
        .await;
    let allocation =
        purchase["data"]["current"]["transaction"]["allocations"][0]["allocation_id"].clone();
    let tail = json!([
        {"op":"create","transaction":{"occurred_on":"2026-09-06","description":"超市部分退货","merchant":"社区超市","movement":{"kind":"refund","original_transaction_id":purchase_id,"receipt_account_id":bank,"money":{"currency":"CNY","amount_minor":"2800"},"original_money":{"currency":"CNY","amount_minor":"2800"},"refund_allocations":[{"allocation_id":allocation,"amount_minor":"2800"}],"posted_on":"2026-09-06"},"evidence_refs":evidence(3),"identifiers":[{"namespace":"synthetic-bank","account_id":bank,"event_type":"booked","external_id":"TEST-REFUND"}]}},
        {"op":"create","transaction":{"occurred_on":"2026-09-10","description":"转入本人储蓄卡，已确认当日到账","movement":{"kind":"transfer","payment_account_id":bank,"receipt_account_id":savings,"from_money":{"currency":"CNY","amount_minor":"200000"},"to_money":{"currency":"CNY","amount_minor":"200000"},"payment_posted_on":"2026-09-10","receipt_posted_on":"2026-09-10"},"evidence_refs":evidence(4),"identifiers":[{"namespace":"synthetic-bank","account_id":bank,"event_type":"booked","external_id":"TEST-TRANSFER"}]}}
    ]);
    let tail_request =
        json!({"ledger_id":ledger,"idempotency_key":"refund-transfer","actions":tail});
    let posted = client
        .call("sumi_transactions_apply", tail_request.clone())
        .await;
    assert_eq!(
        client.call("sumi_transactions_apply", tail_request).await,
        posted
    );
    let repeated = client
        .call(
            "sumi_transactions_apply",
            json!({"ledger_id":ledger,"idempotency_key":"repeat-first-three","actions":actions}),
        )
        .await;
    assert_eq!(
        repeated["changed"], false,
        "Reimport must reuse strong identifiers"
    );
    let report = client.call("sumi_reports_get", json!({"ledger_id":ledger,"report":"income_expense","from":"2026-09-01","before":"2026-10-01"})).await;
    let totals = &report["data"]["totals"][0];
    assert_eq!(totals["income_minor"], "1000000");
    assert_eq!(totals["gross_expense_minor"], "16300");
    assert_eq!(totals["refund_minor"], "2800");
    assert_eq!(totals["net_expense_minor"], "13500");
    let balances = client
        .call(
            "sumi_reports_get",
            json!({"ledger_id":ledger,"report":"account_balances"}),
        )
        .await;
    let balance = |account: &Value| {
        balances["data"]["accounts"]
            .as_array()
            .unwrap()
            .iter()
            .find(|a| &a["account"]["id"] == account)
            .unwrap()["balance_minor"]
            .clone()
    };
    assert_eq!(balance(bank), "1286500");
    assert_eq!(balance(savings), "200000");
    let reconciliation = client.call("sumi_reconcile", json!({"ledger_id":ledger,"account_id":bank,"currency":"CNY","period_start":"2026-09-01","period_end":"2026-10-01","statement_observation_id":observed["data"]["observations"][5]["observation"]["id"],"statement_version":1})).await;
    assert_eq!(
        reconciliation["data"]["reconciliation"]["status"], "reconciled",
        "{reconciliation}"
    );
    assert_eq!(
        reconciliation["data"]["import_completeness"]["status"], "complete",
        "{reconciliation}"
    );
    eprintln!(
        "HTTP MCP statement import passed: 5 raw rows; income CNY 10000.00; gross expense 163.00; refund 28.00; net expense 135.00; bank 12865.00; savings 2000.00; reconciled; complete; repeat import unchanged."
    );
    server.abort();
}
