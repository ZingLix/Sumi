use super::*;

pub async fn acceptance(store: &Store, admin: &PgPool) {
    dates_and_unknowns(store).await;
    batching_and_isolation(store, admin).await;
    remaining_reconciliation_cases(store).await;
    backup_roundtrip(store, admin).await;
}
async fn dates_and_unknowns(store: &Store) {
    let (p, ledger) = user(store, "dates").await;
    let bank = account(store, &p, ledger, "bank", "CNY").await;
    opening(store, &p, ledger, bank, "CNY", "100000").await;
    let original = created_id(&create(store, &p, ledger, "original", expense(bank, "10000")).await);
    let mut corrected = expense(bank, "8000");
    corrected["occurred_on"] = json!("2026-01-30");
    revise(store, &p, ledger, "corrected", original, corrected.clone()).await;
    let allocation =
        journal(store, &p, ledger, original).await["data"]["current"]["transaction"]["allocations"]
            [0]["allocation_id"]
            .clone();
    let mut refund = tx(
        json!({"kind":"refund","original_transaction_id":original,"receipt_account_id":bank,"money":{"currency":"CNY","amount_minor":"2000"},"original_money":{"currency":"CNY","amount_minor":"2000"},"refund_allocations":[{"allocation_id":allocation,"amount_minor":"2000"}],"posted_on":"2026-02-16"}),
    );
    refund["occurred_on"] = json!("2026-02-15");
    create(store, &p, ledger, "real-refund", refund).await;
    let jan=store.reports_get(&p,parse(json!({"ledger_id":ledger,"report":"income_expense","from":"2026-01-01","before":"2026-02-01"}))).await.unwrap();
    assert_eq!(jan["data"]["totals"][0]["gross_expense_minor"], "8000");
    assert_eq!(jan["data"]["totals"][0]["refund_minor"], "0");
    let feb=store.reports_get(&p,parse(json!({"ledger_id":ledger,"report":"income_expense","from":"2026-02-01","before":"2026-03-01"}))).await.unwrap();
    assert_eq!(feb["data"]["totals"][0]["gross_expense_minor"], "0");
    assert_eq!(feb["data"]["totals"][0]["refund_minor"], "2000");
    assert_eq!(feb["data"]["totals"][0]["net_expense_minor"], "-2000");
    assert_eq!(balances(store, &p, ledger).await[&bank], "94000");
    corrected["movement"]["money"]["amount_minor"] = json!("7000");
    revise(store, &p, ledger, "correct-again", original, corrected).await;
    let total = report(store, &p, ledger, "income_expense").await;
    assert_eq!(total["data"]["totals"][0]["net_expense_minor"], "5000");
    assert_eq!(
        journal(store, &p, ledger, original).await["data"]["journal"]
            .as_array()
            .unwrap()
            .len(),
        5
    );
    let unknown = serde_json::json!({"occurred_on":null,"occurred_period":"2026-01","description":"Known expense with unknown payment account","movement":{"kind":"expense","payment_account_id":null,"money":{"currency":"CNY","amount_minor":"100"},"posted_on":"2026-01-31"}});
    let unknown_id =
        created_id(&create(store, &p, ledger, "unknown-account-and-day", unknown).await);
    let unknown = journal(store, &p, ledger, unknown_id).await;
    assert_eq!(
        unknown["data"]["current"]["transaction"]["occurred_period"],
        "2026-01"
    );
    assert!(
        unknown["data"]["quality_flags"]
            .as_array()
            .unwrap()
            .contains(&json!("unknown_payment_account"))
    );
    assert!(
        unknown["data"]["quality_flags"]
            .as_array()
            .unwrap()
            .contains(&json!("occurrence_date_unknown"))
    );
    let mut timed = expense(bank, "1");
    timed["occurred_on"] = Value::Null;
    timed["occurred_at"] = json!("2026-01-30T20:00:00Z");
    let timed_id = created_id(&create(store, &p, ledger, "timestamp", timed).await);
    assert_eq!(
        journal(store, &p, ledger, timed_id).await["data"]["current"]["transaction"]["occurred_on"],
        "2026-01-31"
    );
    let src = source(
        store,
        &p,
        ledger,
        "unknown-source",
        "unknown",
        json!({"authorization":"not paid","balance":"1000000"}),
    )
    .await;
    let records=observations(store,&p,ledger,"non-posting-observations",json!([obs(src,"auth",json!({"type":"authorization","money":{"currency":"CNY","amount_minor":"99999"},"description":"Preauthorization"})),obs(src,"missing",json!({"type":"movement"})),obs(src,"available",json!({"type":"balance","account_id":bank,"currency":"CNY","amount_minor":"1000000","as_of":"2026-01-31","balance_kind":"available"}))])).await;
    let authorization = obs_id(&records, 0);
    let missing = obs_id(&records, 1);
    let before = balances(store, &p, ledger).await;
    observations(store,&p,ledger,"ignore-preauth",json!([{"observation_id":authorization,"expected_version":1,"ignored_reason":"No settled payment","reason":"Keep preauthorization as context only"}])).await;
    let changed=observations(store,&p,ledger,"preauth-updated",json!([{"observation_id":authorization,"expected_version":2,"payload":{"type":"authorization","money":{"currency":"CNY","amount_minor":"50000"},"description":"Updated preauthorization"},"reason":"New extraction"}])).await;
    assert!(changed["data"]["observations"][0]["observation"]["ignored_reason"].is_null());
    let mut invalid = expense(bank, "100");
    invalid["evidence_refs"] = json!([evidence(missing, 1)]);
    assert!(
        apply(
            store,
            &p,
            ledger,
            "missing-money-evidence",
            json!([{"op":"create","transaction":invalid}])
        )
        .await
        .is_err()
    );
    assert_eq!(balances(store, &p, ledger).await, before);
    let snapshot = store
        .observations_search(
            &p,
            parse(
                json!({"ledger_id":ledger,"observation_id":authorization,"include_history":true}),
            ),
        )
        .await
        .unwrap();
    assert_eq!(
        snapshot["data"]["items"][0]["history"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
}
async fn batching_and_isolation(store: &Store, admin: &PgPool) {
    let (p, ledger) = user(store, "batch").await;
    let bank = account(store, &p, ledger, "bank", "CNY").await;
    let src = source(
        store,
        &p,
        ledger,
        "source",
        "batch",
        json!({"count":1,"records":[{"locator":"a"}]}),
    )
    .await;
    let batch=store.observations_ingest(&p,parse(json!({"ledger_id":ledger,"idempotency_key":"batch","observations":[obs(src,"a",movement(bank,"CNY","123","out"))],"batch":{"name":"January","source_version_ids":[src],"complete":false}}))).await.unwrap();
    let batch_id = id(&batch["data"]["batch"]["batch"]["id"]);
    assert_eq!(batch["data"]["batch"]["actual_count"], 1);
    assert_eq!(
        batch["data"]["batch"]["actual_totals"][0]["debit_minor"],
        "123"
    );
    let finished=store.observations_ingest(&p,parse(json!({"ledger_id":ledger,"idempotency_key":"finish-batch","observations":[],"batch":{"id":batch_id,"expected_version":1,"name":"January","source_version_ids":[src],"complete":true}}))).await.unwrap();
    assert_eq!(finished["data"]["batch"]["batch"]["version"], 2);
    let unchanged=store.observations_ingest(&p,parse(json!({"ledger_id":ledger,"idempotency_key":"finish-batch-again","observations":[],"batch":{"id":batch_id,"expected_version":2,"name":"January","source_version_ids":[src],"complete":true}}))).await.unwrap();
    assert_eq!(unchanged["changed"], false);
    assert_eq!(finished["ledger_revision"], unchanged["ledger_revision"]);
    let recovered=store.operations_get(&p,parse(json!({"ledger_id":ledger,"tool":"sumi_observations_ingest","idempotency_key":"finish-batch"}))).await.unwrap();
    assert_eq!(recovered["data"]["result"], finished);
    let repeated = observations(
        store,
        &p,
        ledger,
        "same-position-again",
        json!([obs(src, "a", movement(bank, "CNY", "123", "out"))]),
    )
    .await;
    assert_eq!(repeated["changed"], false);
    assert_eq!(obs_id(&repeated, 0), obs_id(&batch, 0));
    let prefs = json!({"ledger_id":ledger,"idempotency_key":"preference-names","expected_version":1,"reason":"Categories","categories":[{"name":"Unchanging","kind":"expense"}],"tags":[{"name":"Unchanging"}]});
    let saved = store
        .preferences_update(&p, parse(prefs.clone()))
        .await
        .unwrap();
    let mut again = prefs;
    again["idempotency_key"] = json!("preference-names-again");
    again["expected_version"] = saved["data"]["preferences"]["version"].clone();
    let repeated = store.preferences_update(&p, parse(again)).await.unwrap();
    assert_eq!(repeated["changed"], false);
    assert_eq!(repeated["ledger_revision"], saved["ledger_revision"]);
    let (other, other_ledger) = user(store, "isolation").await;
    assert!(
        store
            .sources_get(
                &other,
                parse(json!({"ledger_id":other_ledger,"source_version_id":src}))
            )
            .await
            .is_err()
    );
    assert!(store.fx_rates_ingest(&other,parse(json!({"ledger_id":other_ledger,"idempotency_key":"foreign-source","rates":[{"from_currency":"USD","to_currency":"CNY","on":"2026-01-30","rate":"7.3","source_version_id":src,"record_locator":"$"}]}))).await.is_err());
    assert!(store.observations_ingest(&other,parse(json!({"ledger_id":other_ledger,"idempotency_key":"foreign-observation","observations":[{"observation_id":obs_id(&batch,0),"expected_version":1,"review_note":"intruder","reason":"bad"}]}))).await.is_err());
    for table in [
        "sources",
        "source_versions",
        "observations",
        "observation_versions",
        "evidence_links",
        "fx_rates",
        "preferences",
        "categories",
        "tags",
        "authorization_grants",
        "operations",
    ] {
        let mut session = store.pool.begin().await.unwrap();
        sqlx::query("SELECT set_config('sumi.user_id',$1,true)")
            .bind(other.user_id().to_string())
            .execute(&mut *session)
            .await
            .unwrap();
        let count: i64 =
            sqlx::query_scalar(&format!("SELECT count(*) FROM {table} WHERE ledger_id=$1"))
                .bind(ledger)
                .fetch_one(&mut *session)
                .await
                .unwrap();
        assert_eq!(count, 0, "RLS leaked {table}");
        session.rollback().await.unwrap();
    }
    sqlx::query(
        "UPDATE authorization_grants SET status='revoked' WHERE ledger_id=$1 AND client_id=$2",
    )
    .bind(ledger)
    .bind(&p.client_id)
    .execute(admin)
    .await
    .unwrap();
    assert!(matches!(store.context(&p).await, Err(Error::Forbidden)));
    sqlx::query("UPDATE authorization_grants SET status='active',scopes=ARRAY['sumi:read'] WHERE ledger_id=$1 AND client_id=$2").bind(ledger).bind(&p.client_id).execute(admin).await.unwrap();
    assert!(store.context(&p).await.is_ok());
    assert!(matches!(store.accounts_upsert(&p,parse(json!({"ledger_id":ledger,"idempotency_key":"restricted-grant","name":"not allowed","kind":"asset","currency":"CNY"}))).await,Err(Error::Forbidden)));
    sqlx::query("UPDATE authorization_grants SET scopes=ARRAY['sumi:read','sumi:write'] WHERE ledger_id=$1 AND client_id=$2").bind(ledger).bind(&p.client_id).execute(admin).await.unwrap();
    sqlx::query("UPDATE users SET status='disabled' WHERE id=$1")
        .bind(p.user_id())
        .execute(admin)
        .await
        .unwrap();
    assert!(matches!(store.context(&p).await, Err(Error::Forbidden)));
    sqlx::query("UPDATE users SET status='active' WHERE id=$1")
        .bind(p.user_id())
        .execute(admin)
        .await
        .unwrap();
}
async fn remaining_reconciliation_cases(store: &Store) {
    let (p, ledger) = user(store, "matrix-extra").await;
    let bank = account(store, &p, ledger, "bank", "CNY").await;
    opening(store, &p, ledger, bank, "CNY", "100000").await;
    let src = source(
        store,
        &p,
        ledger,
        "source",
        "matrix",
        json!({"count":2,"records":[{"locator":"d"},{"locator":"c"}]}),
    )
    .await;
    let mut only_count = statement(bank);
    only_count["expected_debit_total"] = Value::Null;
    only_count["expected_credit_total"] = Value::Null;
    only_count["record_locators"] = Value::Null;
    only_count["range_locator"] = json!("$");
    let saved = observations(
        store,
        &p,
        ledger,
        "matrix-rows",
        json!([
            obs(src, "statement", only_count.clone()),
            obs(src, "d", movement(bank, "CNY", "10000", "out")),
            obs(src, "c", movement(bank, "CNY", "10000", "in"))
        ]),
    )
    .await;
    let stmt = obs_id(&saved, 0);
    let mut out = expense(bank, "10000");
    out["evidence_refs"] = json!([evidence(obs_id(&saved, 1), 1)]);
    let out_id = created_id(&create(store, &p, ledger, "out", out).await);
    let mut incoming = tx(
        json!({"kind":"income","receipt_account_id":bank,"money":{"currency":"CNY","amount_minor":"10000"},"posted_on":"2026-01-31"}),
    );
    incoming["evidence_refs"] = json!([evidence(obs_id(&saved, 2), 1)]);
    create(store, &p, ledger, "in", incoming).await;
    let full = check(store, &p, ledger, bank, stmt).await;
    assert_eq!(full["data"]["reconciliation"]["status"], "reconciled");
    assert_eq!(full["data"]["import_completeness"]["status"], "complete");
    assert_eq!(
        full["data"]["import_completeness"]["controls"]["debit_total"]["status"],
        "unavailable"
    );
    let mut inferred = current(&journal(store, &p, ledger, out_id).await);
    inferred["inferred_posting_accounts"] = json!([bank]);
    revise(
        store,
        &p,
        ledger,
        "inferred-posting",
        out_id,
        inferred.clone(),
    )
    .await;
    let unclear = check(store, &p, ledger, bank, stmt).await;
    assert_eq!(unclear["data"]["reconciliation"]["status"], "incomplete");
    assert_eq!(unclear["data"]["import_completeness"]["status"], "complete");
    let quality = report(store, &p, ledger, "data_quality").await;
    let old = quality["data"]["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["ledger_revision"] == full["ledger_revision"])
        .unwrap();
    assert_eq!(old["data"]["reconciliation"]["stale"], true);
    assert_eq!(old["data"]["import_completeness"]["stale"], true);
    inferred["inferred_posting_accounts"] = json!([]);
    revise(store, &p, ledger, "known-posting", out_id, inferred).await;
    let mut mismatched = only_count.clone();
    mismatched["closing_balance"] = json!("110000");
    observations(store,&p,ledger,"mismatch",json!([{"observation_id":stmt,"expected_version":1,"payload":mismatched,"reason":"Statement differs from ledger"}])).await;
    let mismatch = check(store, &p, ledger, bank, stmt).await;
    assert_eq!(mismatch["data"]["reconciliation"]["status"], "mismatch");
    assert_eq!(
        mismatch["data"]["import_completeness"]["status"],
        "complete"
    );
    let mut partial = only_count.clone();
    partial["source_coverage"] = json!("partial");
    observations(store,&p,ledger,"partial",json!([{"observation_id":stmt,"expected_version":2,"payload":partial,"reason":"Some material missing"}])).await;
    let partial = check(store, &p, ledger, bank, stmt).await;
    assert_eq!(partial["data"]["reconciliation"]["status"], "reconciled");
    assert_eq!(
        partial["data"]["import_completeness"]["status"],
        "incomplete"
    );
    let cleared=observations(store,&p,ledger,"clear-mistaken-control",json!([{"observation_id":stmt,"expected_version":3,"payload":only_count,"clear_controls":["count"],"control_correction_locator":"/records","reason":"Count was incorrectly extracted; preserve correction locator"}])).await;
    assert!(
        cleared["data"]["observations"][0]["observation"]["payload"]["expected_count"].is_null()
    );
    let usd = account(store, &p, ledger, "usd", "USD").await;
    let observation = obs_id(&saved, 1);
    let changed=observations(store,&p,ledger,"different-facts",json!([{"observation_id":observation,"expected_version":1,"payload":movement(usd,"USD","100","in"),"reason":"Account, currency and direction were incorrect"}])).await;
    assert_eq!(
        changed["data"]["observations"][0]["status"]["fact_conflict"],
        true
    );
    assert_eq!(
        changed["data"]["observations"][0]["status"]["needs_review"],
        true
    );
    let mut corrected = tx(
        json!({"kind":"income","receipt_account_id":usd,"money":{"currency":"USD","amount_minor":"100"},"posted_on":"2026-01-31"}),
    );
    corrected["evidence_refs"] = json!([evidence(observation, 2)]);
    revise(
        store,
        &p,
        ledger,
        "resolve-changed-facts",
        out_id,
        corrected,
    )
    .await;
    assert_eq!(balances(store, &p, ledger).await[&usd], "100");
}
async fn backup_roundtrip(store: &Store, admin: &PgPool) {
    let Ok(bin) = std::env::var("SUMI_TEST_PG_BIN") else {
        return;
    };
    let dir = std::env::temp_dir().join(format!("sumi_backup_{}", Uuid::new_v4().simple()));
    std::fs::create_dir(&dir).unwrap();
    let dump = dir.join("backup.dump");
    let db_name: String = sqlx::query_scalar("SELECT current_database()::text")
        .fetch_one(admin)
        .await
        .unwrap();
    let mut url = url::Url::parse(&std::env::var("SUMI_TEST_ADMIN_URL").unwrap()).unwrap();
    url.set_path(&db_name);
    let backup = std::process::Command::new("sh")
        .arg(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/backup.sh"))
        .arg(&dump)
        .env("DATABASE_URL", url.as_str())
        .env("SUMI_PG_DUMP", std::path::Path::new(&bin).join("pg_dump"))
        .output()
        .unwrap();
    assert!(
        backup.status.success(),
        "pg_dump: {}",
        String::from_utf8_lossy(&backup.stderr)
    );
    let name = format!("sumi_backup_restore_{}", Uuid::new_v4().simple());
    sqlx::query(&format!("CREATE DATABASE {name}"))
        .execute(admin)
        .await
        .unwrap();
    url.set_path(&name);
    let restore = std::process::Command::new(std::path::Path::new(&bin).join("pg_restore"))
        .args([
            "--no-owner",
            "--no-acl",
            "--exit-on-error",
            "--dbname",
            url.as_str(),
        ])
        .arg(&dump)
        .output()
        .unwrap();
    assert!(
        restore.status.success(),
        "pg_restore: {}",
        String::from_utf8_lossy(&restore.stderr)
    );
    let target = PgPool::connect_with(admin.connect_options().as_ref().clone().database(&name))
        .await
        .unwrap();
    let role: String = sqlx::query_scalar("SELECT current_user::text")
        .fetch_one(&store.pool)
        .await
        .unwrap();
    sqlx::raw_sql(&include_str!("../../scripts/grant-runtime.sql").replace("sumi_app", &role))
        .execute(&target)
        .await
        .unwrap();
    let restored_pool = PgPool::connect_with(
        store
            .pool
            .connect_options()
            .as_ref()
            .clone()
            .database(&name),
    )
    .await
    .unwrap();
    let restored = Store::new(restored_pool.clone());
    restored.verify_runtime_role().await.unwrap();
    let (p, ledger) = user(store, "evidence").await;
    assert_eq!(
        report(&restored, &p, ledger, "export").await["data"]["content_hash"],
        report(store, &p, ledger, "export").await["data"]["content_hash"]
    );
    let row=sqlx::query("SELECT e.id,l.account_id,l.currency FROM journal_entries e JOIN journal_lines l ON l.ledger_id=e.ledger_id AND l.entry_id=e.id WHERE e.ledger_id=$1 LIMIT 1").bind(ledger).fetch_one(&target).await.unwrap();
    assert!(sqlx::query("INSERT INTO journal_lines(ledger_id,entry_id,line_number,account_id,currency,amount_minor) VALUES($1,$2,1000,$3,$4,1)").bind(ledger).bind(row.get::<Uuid,_>("id")).bind(row.get::<Uuid,_>("account_id")).bind(row.get::<String,_>("currency")).execute(&target).await.is_err());
    restored_pool.close().await;
    target.close().await;
    sqlx::query(&format!("DROP DATABASE {name} WITH (FORCE)"))
        .execute(admin)
        .await
        .unwrap();
    std::fs::remove_file(&dump).unwrap();
    std::fs::remove_dir(dir).unwrap();
}
