mod common;
mod mvp;
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use sqlx::{PgPool, Row};
use std::collections::BTreeMap;
use sumi::{api::*, auth::Principal, error::Error, server, store::Store};
use tower::ServiceExt;
use uuid::Uuid;

fn parse<T: DeserializeOwned>(v: Value) -> T {
    serde_json::from_value(v).unwrap()
}
fn id(v: &Value) -> Uuid {
    v.as_str().unwrap().parse().unwrap()
}
fn tx(data: Value) -> Value {
    json!({"occurred_on":"2026-01-30","description":"test","merchant":"shop","review_note":null,"movement":data})
}
fn expense(account: Uuid, amount: &str) -> Value {
    tx(
        json!({"kind":"expense","payment_account_id":account,"money":{"currency":"CNY","amount_minor":amount},"posted_on":"2026-01-31"}),
    )
}
async fn apply(
    store: &Store,
    p: &Principal,
    ledger: Uuid,
    key: &str,
    actions: Value,
) -> sumi::error::Result<Value> {
    store
        .transactions_apply(
            p,
            parse(json!({"ledger_id":ledger,"idempotency_key":key,"actions":actions})),
        )
        .await
}
async fn account(store: &Store, p: &Principal, ledger: Uuid, name: &str, currency: &str) -> Uuid {
    let result=store.accounts_upsert(p,parse(json!({"ledger_id":ledger,"idempotency_key":name,"name":name,"kind":"asset","currency":currency}))).await.unwrap();
    id(&result["data"]["account"]["id"])
}
async fn journal(store: &Store, p: &Principal, ledger: Uuid, transaction: Uuid) -> Value {
    store
        .transactions_get(
            p,
            TransactionsGet {
                ledger_id: ledger,
                transaction_id: transaction,
            },
        )
        .await
        .unwrap()
}
async fn report(store: &Store, p: &Principal, ledger: Uuid, kind: &str) -> Value {
    store
        .reports_get(p, parse(json!({"ledger_id":ledger,"report":kind})))
        .await
        .unwrap()
}
async fn rpc(router: axum::Router, subject: &str, body: impl Serialize) -> Value {
    let token = common::token(&common::claims(subject));
    let response = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/mcp")
                .header("host", "127.0.0.1:8080")
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .header("accept", "application/json, text/event-stream")
                .header("mcp-protocol-version", "2025-11-25")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(
        status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&bytes)
    );
    serde_json::from_slice(&bytes).unwrap()
}

/// Creates and drops a fresh database and role. Never migrates an existing DB.
#[tokio::test]
#[ignore = "requires local PostgreSQL admin; run with SUMI_TEST_ADMIN_URL and --ignored"]
async fn postgres_ledger_acceptance() {
    let admin_url = std::env::var("SUMI_TEST_ADMIN_URL")
        .expect("Set SUMI_TEST_ADMIN_URL to a disposable local PostgreSQL cluster");
    let parsed = url::Url::parse(&admin_url).unwrap();
    assert!(
        matches!(parsed.host_str(), Some("localhost" | "127.0.0.1" | "[::1]")),
        "Integration tests only use loopback PostgreSQL"
    );
    let cluster = PgPool::connect(&admin_url).await.unwrap();
    let name = format!("sumi_test_{}", Uuid::new_v4().simple());
    let role = format!("sumi_app_{}", Uuid::new_v4().simple());
    sqlx::query(&format!("CREATE DATABASE {name}"))
        .execute(&cluster)
        .await
        .unwrap();
    sqlx::query(&format!(
        "CREATE ROLE {role} LOGIN PASSWORD '{role}' NOSUPERUSER NOBYPASSRLS"
    ))
    .execute(&cluster)
    .await
    .unwrap();
    let mut db_url = parsed.clone();
    db_url.set_path(&name);
    let admin = PgPool::connect(db_url.as_str()).await.unwrap();
    sqlx::migrate!().run(&admin).await.unwrap();
    sqlx::raw_sql(&include_str!("../scripts/grant-runtime.sql").replace("sumi_app", &role))
        .execute(&admin)
        .await
        .unwrap();
    db_url.set_username(&role).unwrap();
    db_url.set_password(Some(&role)).unwrap();
    let pool = PgPool::connect(db_url.as_str()).await.unwrap();
    let store = Store::new(pool.clone());
    store.verify_runtime_role().await.unwrap();
    assert!(
        Store::new(admin.clone())
            .verify_runtime_role()
            .await
            .is_err()
    );

    let alice = common::principal("alice");
    let bob = common::principal("bob");
    let context = store.context(&alice).await.unwrap();
    let ledger = id(&context["data"]["ledger_id"]);
    assert_eq!(store.context(&alice).await.unwrap(), context);
    let bob_ledger = id(&store.context(&bob).await.unwrap()["data"]["ledger_id"]);
    assert_ne!(ledger, bob_ledger);
    assert!(matches!(
        store
            .accounts_list(&bob, parse(json!({"ledger_id":ledger})))
            .await,
        Err(Error::NotFound)
    ));
    let mut read_only = alice.clone();
    read_only.scopes.remove("sumi:write");
    assert!(matches!(store.accounts_upsert(&read_only,parse(json!({"ledger_id":ledger,"idempotency_key":"denied","name":"no","kind":"asset","currency":"CNY"}))).await,Err(Error::Forbidden)));

    let bank = account(&store, &alice, ledger, "bank", "CNY").await;
    let usd = account(&store, &alice, ledger, "usd", "USD").await;
    let opening=apply(&store,&alice,ledger,"opening",json!([
        {"op":"set_opening","account_id":bank,"money":{"currency":"CNY","amount_minor":"100000"},"as_of":"2025-12-31","reason":"Confirmed statement"},
        {"op":"set_opening","account_id":usd,"money":{"currency":"USD","amount_minor":"0"},"as_of":"2025-12-31","reason":"Confirmed zero"}
    ])).await.unwrap();
    assert_eq!(
        opening["data"]["transactions"][1]["appended_entries"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
    let create = json!([{"op":"create","transaction":expense(bank,"10000")}]);
    let first = apply(&store, &alice, ledger, "purchase", create.clone())
        .await
        .unwrap();
    let transaction = id(&first["data"]["transactions"][0]["transaction_id"]);
    assert_eq!(
        apply(&store, &alice, ledger, "purchase", create)
            .await
            .unwrap(),
        first
    );
    assert!(matches!(
        apply(
            &store,
            &alice,
            ledger,
            "purchase",
            json!([{"op":"create","transaction":expense(bank,"1000")}])
        )
        .await,
        Err(Error::Idempotency)
    ));

    let revision = first["ledger_revision"].as_i64().unwrap();
    let preview=store.transactions_apply(&alice,parse(json!({"ledger_id":ledger,"idempotency_key":"preview","preview":true,"actions":[{"op":"create","transaction":expense(bank,"100")}]}))).await.unwrap();
    assert_eq!(preview["changed"], false);
    assert_eq!(preview["data"]["would_change"], true);
    assert_eq!(preview["ledger_revision"], revision);
    assert_eq!(
        store.context(&alice).await.unwrap()["ledger_revision"],
        revision
    );
    let invalid_batch = json!([{"op":"create","transaction":expense(bank,"10")},{"op":"create","transaction":expense(Uuid::new_v4(),"20")}]);
    assert!(
        apply(&store, &alice, ledger, "rollback", invalid_batch)
            .await
            .is_err()
    );
    assert_eq!(
        store.context(&alice).await.unwrap()["ledger_revision"],
        revision
    );

    let correction = json!([{"op":"revise","transaction_id":transaction,"expected_version":1,"reason":"Receipt says 80","transaction":expense(bank,"8000")}]);
    let fixed = apply(&store, &alice, ledger, "correction", correction)
        .await
        .unwrap();
    assert_eq!(fixed["data"]["transactions"][0]["version"], 2);
    let history = journal(&store, &alice, ledger, transaction).await;
    assert_eq!(history["data"]["journal"].as_array().unwrap().len(), 3);
    assert!(
        history["data"]["journal"]
            .as_array()
            .unwrap()
            .iter()
            .all(|e| e["effective_on"] == "2026-01-31")
    );
    let stats = report(&store, &alice, ledger, "income_expense").await;
    assert_eq!(stats["data"]["totals"][0]["gross_expense_minor"], "8000");
    assert_eq!(stats["data"]["totals"][0]["refund_minor"], "0");
    let mut noted = expense(bank, "8000");
    noted["review_note"] = json!("Two similar purchases are independent");
    apply(&store,&alice,ledger,"note",json!([{"op":"revise","transaction_id":transaction,"expected_version":2,"reason":"Save note","transaction":noted}])).await.unwrap();
    let history = journal(&store, &alice, ledger, transaction).await;
    assert_eq!(history["data"]["journal"].as_array().unwrap().len(), 3);
    let before_noop = history["ledger_revision"].clone();
    let noop=apply(&store,&alice,ledger,"noop",json!([{"op":"revise","transaction_id":transaction,"expected_version":3,"reason":"Different reason only","transaction":noted}])).await.unwrap();
    assert_eq!(noop["changed"], false);
    assert_eq!(noop["ledger_revision"], before_noop);
    assert!(matches!(apply(&store,&alice,ledger,"old-version",json!([{"op":"revise","transaction_id":transaction,"expected_version":2,"reason":"stale","transaction":noted}])).await,Err(Error::Version{..})));
    let recovered = store
        .operations_get(
            &alice,
            OperationsGet {
                ledger_id: ledger,
                operation_id: Some(id(&noop["operation_id"])),
                idempotency_key: None,
                tool: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(recovered["data"]["result"], noop);

    // Financial corrections preserve posting dates and only reverse active business entries.
    let mut moved = noted.clone();
    moved["movement"]["posted_on"] = json!("2026-02-01");
    apply(&store,&alice,ledger,"move-posting",json!([{"op":"revise","transaction_id":transaction,"expected_version":3,"reason":"Actual posting date","transaction":moved}])).await.unwrap();
    moved["movement"]["money"]["amount_minor"] = json!("7000");
    apply(&store,&alice,ledger,"correct-again",json!([{"op":"revise","transaction_id":transaction,"expected_version":4,"reason":"Final receipt","transaction":moved}])).await.unwrap();
    assert_eq!(
        journal(&store, &alice, ledger, transaction).await["data"]["journal"]
            .as_array()
            .unwrap()
            .len(),
        7
    );
    let jan_balances = store
        .accounts_list(
            &alice,
            parse(json!({"ledger_id":ledger,"as_of":"2026-01-31"})),
        )
        .await
        .unwrap();
    let bank_balance = jan_balances["data"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["account"]["id"] == bank.to_string())
        .unwrap();
    assert_eq!(bank_balance["balance_minor"], "100000");

    // Complete a one-sided transfer in the same transaction identity.
    let partial=apply(&store,&alice,ledger,"transit",json!([{"op":"create","transaction":tx(json!({"kind":"unresolved_movement","account_id":bank,"money":{"currency":"CNY","amount_minor":"72000"},"direction":"out","posted_on":"2026-01-31","transfer_in_transit":true}))}])).await.unwrap();
    let transfer_id = id(&partial["data"]["transactions"][0]["transaction_id"]);
    let transfer = tx(
        json!({"kind":"transfer","payment_account_id":bank,"receipt_account_id":usd,"from_money":{"currency":"CNY","amount_minor":"72000"},"to_money":{"currency":"USD","amount_minor":"10000"},"payment_posted_on":"2026-01-31","receipt_posted_on":"2026-02-02"}),
    );
    apply(&store,&alice,ledger,"complete-transfer",json!([{"op":"revise","transaction_id":transfer_id,"expected_version":1,"reason":"Receipt received","transaction":transfer}])).await.unwrap();
    let transfer_noop=apply(&store,&alice,ledger,"complete-again",json!([{"op":"revise","transaction_id":transfer_id,"expected_version":2,"reason":"Retry","transaction":transfer}])).await.unwrap();
    assert_eq!(transfer_noop["changed"], false);
    let balances = report(&store, &alice, ledger, "account_balances").await;
    let items = balances["data"]["accounts"].as_array().unwrap();
    assert_eq!(
        items
            .iter()
            .find(|a| a["account"]["id"] == bank.to_string())
            .unwrap()["balance_minor"],
        "21000"
    );
    assert_eq!(
        items
            .iter()
            .find(|a| a["account"]["id"] == usd.to_string())
            .unwrap()["balance_minor"],
        "10000"
    );
    assert_eq!(
        items
            .iter()
            .find(|a| a["account"]["role"] == "transit")
            .unwrap()["balance_minor"],
        "0"
    );
    assert_eq!(
        report(&store, &alice, ledger, "income_expense").await["data"]["totals"][0]["gross_expense_minor"],
        "7000"
    );

    // Same-day FX metadata changes must not regenerate either currency's journal.
    let mut same_day = transfer.clone();
    same_day["movement"]["receipt_posted_on"] = json!("2026-01-31");
    let created = apply(
        &store,
        &alice,
        ledger,
        "same-day-fx",
        json!([{"op":"create","transaction":same_day}]),
    )
    .await
    .unwrap();
    let same_day_id = id(&created["data"]["transactions"][0]["transaction_id"]);
    same_day["occurred_on"] = json!("2026-02-01");
    apply(&store,&alice,ledger,"fx-occurrence-only",json!([{"op":"revise","transaction_id":same_day_id,"expected_version":1,"reason":"Occurrence date only","transaction":same_day}])).await.unwrap();
    assert_eq!(
        journal(&store, &alice, ledger, same_day_id).await["data"]["journal"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let voided=apply(&store,&alice,ledger,"void-fx",json!([{"op":"void","transaction_id":same_day_id,"expected_version":2,"reason":"Duplicate transaction"}])).await.unwrap();
    assert_eq!(voided["data"]["transactions"][0]["state"], "voided");
    let void_noop = apply(
        &store,
        &alice,
        ledger,
        "void-fx-again",
        json!([{"op":"void","transaction_id":same_day_id,"expected_version":3,"reason":"Retry"}]),
    )
    .await
    .unwrap();
    assert_eq!(void_noop["changed"], false);
    assert_eq!(
        journal(&store, &alice, ledger, same_day_id).await["data"]["journal"]
            .as_array()
            .unwrap()
            .len(),
        4
    );

    // A closed account retains its history and remains correctable.
    let closed = account(&store, &alice, ledger, "closed-account", "CNY").await;
    let old = apply(
        &store,
        &alice,
        ledger,
        "closed-purchase",
        json!([{"op":"create","transaction":expense(closed,"10")}]),
    )
    .await
    .unwrap();
    let old_id = id(&old["data"]["transactions"][0]["transaction_id"]);
    store.accounts_upsert(&alice,parse(json!({"ledger_id":ledger,"idempotency_key":"deactivate","account_id":closed,"expected_version":1,"name":"closed-account","kind":"asset","currency":"CNY","active":false}))).await.unwrap();
    assert!(
        apply(
            &store,
            &alice,
            ledger,
            "closed-new",
            json!([{"op":"create","transaction":expense(closed,"10")}])
        )
        .await
        .is_err()
    );
    let mut historical = expense(closed, "10");
    historical["review_note"] = json!("Historical note");
    apply(&store,&alice,ledger,"closed-note",json!([{"op":"revise","transaction_id":old_id,"expected_version":1,"reason":"Annotate historical transaction","transaction":historical}])).await.unwrap();
    assert_eq!(
        journal(&store, &alice, ledger, old_id).await["data"]["journal"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    // Concurrent retry is serialized by the per-ledger lock.
    let concurrent = json!([{"op":"create","transaction":expense(bank,"100")}]);
    let (one, two) = tokio::join!(
        apply(&store, &alice, ledger, "concurrent", concurrent.clone()),
        apply(&store, &alice, ledger, "concurrent", concurrent)
    );
    assert_eq!(one.unwrap(), two.unwrap());
    let mut mutation = json!([{"op":"revise","transaction_id":transaction,"expected_version":5,"reason":"Concurrent update","transaction":moved}]);
    mutation[0]["transaction"]["review_note"] = json!("first writer");
    let mut mutation2 = mutation.clone();
    mutation2[0]["transaction"]["review_note"] = json!("another writer");
    let (one, two) = tokio::join!(
        apply(&store, &alice, ledger, "writer-one", mutation),
        apply(&store, &alice, ledger, "writer-two", mutation2)
    );
    assert_ne!(one.is_ok(), two.is_ok());
    assert!(matches!(
        one.err().or_else(|| two.err()),
        Some(Error::Version { .. })
    ));

    // A cursor from an older ledger revision cannot silently skip rows.
    let first_page = store
        .accounts_list(&alice, parse(json!({"ledger_id":ledger,"limit":1})))
        .await
        .unwrap();
    let cursor = first_page["data"]["next_cursor"].as_str().unwrap();
    account(&store, &alice, ledger, "wallet", "CNY").await;
    assert!(matches!(
        store
            .accounts_list(
                &alice,
                parse(json!({"ledger_id":ledger,"limit":1,"cursor":cursor}))
            )
            .await,
        Err(Error::CursorStale)
    ));

    // Runtime SQL cannot see another user, and connection-local identity does not leak.
    let mut rls = pool.begin().await.unwrap();
    sqlx::query("SELECT set_config('sumi.user_id',$1,true)")
        .bind(bob.user_id().to_string())
        .execute(&mut *rls)
        .await
        .unwrap();
    let visible: i64 = sqlx::query_scalar("SELECT count(*) FROM accounts WHERE ledger_id=$1")
        .bind(ledger)
        .fetch_one(&mut *rls)
        .await
        .unwrap();
    assert_eq!(visible, 0);
    assert!(sqlx::query("INSERT INTO accounts(ledger_id,id,name,kind,currency) VALUES($1,$2,'bad','asset','CNY')").bind(ledger).bind(Uuid::new_v4()).execute(&mut *rls).await.is_err());
    rls.rollback().await.unwrap();
    let visible: i64 = sqlx::query_scalar("SELECT count(*) FROM accounts")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(visible, 0);
    for bad in [
        "UPDATE journal_lines SET amount_minor=amount_minor",
        "DELETE FROM journal_entries",
        "TRUNCATE journal_entries",
    ] {
        assert!(sqlx::query(bad).execute(&pool).await.is_err());
    }
    assert!(
        sqlx::query("SELECT 1.5::minor_amount")
            .execute(&admin)
            .await
            .is_err()
    );
    let entry=sqlx::query("SELECT e.id,l.account_id,l.currency FROM journal_entries e JOIN journal_lines l ON e.ledger_id=l.ledger_id AND e.id=l.entry_id WHERE e.ledger_id=$1 LIMIT 1").bind(ledger).fetch_one(&admin).await.unwrap();
    assert!(sqlx::query("INSERT INTO journal_lines(ledger_id,entry_id,line_number,account_id,currency,amount_minor) VALUES($1,$2,99,$3,$4,1)").bind(ledger).bind(entry.get::<Uuid,_>("id")).bind(entry.get::<Uuid,_>("account_id")).bind(entry.get::<String,_>("currency")).execute(&admin).await.is_err());

    assert!(
        sqlx::query("UPDATE journal_lines SET amount_minor=amount_minor WHERE ledger_id=$1")
            .bind(ledger)
            .execute(&admin)
            .await
            .is_err()
    );
    let template=sqlx::query("SELECT transaction_id,transaction_version,operation_id,recorded_revision FROM journal_entries WHERE ledger_id=$1 LIMIT 1").bind(ledger).fetch_one(&admin).await.unwrap();
    // The database checks empty entries, unequal sums and cross-currency cancellation at commit.
    for mode in 0..3 {
        let mut raw = admin.begin().await.unwrap();
        let entry_id = Uuid::new_v4();
        sqlx::query("INSERT INTO journal_entries(ledger_id,id,transaction_id,transaction_version,effective_on,recorded_revision,operation_id) VALUES($1,$2,$3,$4,'2026-01-31',$5,$6)")
            .bind(ledger).bind(entry_id).bind(template.get::<Uuid,_>("transaction_id")).bind(template.get::<i64,_>("transaction_version")).bind(template.get::<i64,_>("recorded_revision")).bind(template.get::<Uuid,_>("operation_id")).execute(&mut *raw).await.unwrap();
        if mode > 0 {
            sqlx::query("INSERT INTO journal_lines(ledger_id,entry_id,line_number,account_id,currency,amount_minor) VALUES($1,$2,1,$3,'CNY',100)").bind(ledger).bind(entry_id).bind(bank).execute(&mut *raw).await.unwrap();
            let (other, currency, amount) = if mode == 1 {
                (bank, "CNY", -99)
            } else {
                (usd, "USD", -100)
            };
            sqlx::query("INSERT INTO journal_lines(ledger_id,entry_id,line_number,account_id,currency,amount_minor) VALUES($1,$2,2,$3,$4,$5)").bind(ledger).bind(entry_id).bind(other).bind(currency).bind(bigdecimal::BigDecimal::from(amount)).execute(&mut *raw).await.unwrap();
        }
        assert!(
            raw.commit().await.is_err(),
            "database accepted invalid journal mode {mode}"
        );
    }

    // Real rmcp transport: initialize, discover schemas, invoke tools, reauthorize every call.
    let router = server::router(store.clone(), common::auth());
    let initialized=rpc(router.clone(),"alice",json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"test","version":"1"}}})).await;
    assert_eq!(initialized["result"]["serverInfo"]["name"], "sumi");
    let tools = rpc(
        router.clone(),
        "alice",
        json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}),
    )
    .await;
    assert_eq!(tools["result"]["tools"].as_array().unwrap().len(), 16);
    assert!(
        tools["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .all(|t| t["inputSchema"].is_object())
    );
    let ctx=rpc(router.clone(),"alice",json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"sumi_context_get","arguments":{}}})).await;
    assert_eq!(
        ctx["result"]["structuredContent"]["data"]["ledger_id"],
        ledger.to_string()
    );
    let posted=rpc(router.clone(),"alice",json!({"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"sumi_transactions_apply","arguments":{"ledger_id":ledger,"idempotency_key":"http-purchase","actions":[{"op":"create","transaction":expense(bank,"100")} ]}}})).await;
    assert_eq!(posted["result"]["structuredContent"]["changed"], true);
    let denied=rpc(router,"bob",json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"sumi_accounts_list","arguments":{"ledger_id":ledger}}})).await;
    assert_eq!(denied["result"]["isError"], true);

    mvp::acceptance(&store, &admin).await;
    pool.close().await;
    admin.close().await;
    sqlx::query(&format!("DROP DATABASE {name} WITH (FORCE)"))
        .execute(&cluster)
        .await
        .unwrap();
    sqlx::query(&format!("DROP ROLE {role}"))
        .execute(&cluster)
        .await
        .unwrap();
}
