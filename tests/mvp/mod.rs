use super::*;
mod edges;
mod mcp_import;

async fn user(store: &Store, name: &str) -> (Principal, Uuid) {
    let p = common::principal(name);
    let ledger = id(&store.context(&p).await.unwrap()["data"]["ledger_id"]);
    (p, ledger)
}
async fn opening(
    store: &Store,
    p: &Principal,
    ledger: Uuid,
    account: Uuid,
    currency: &str,
    amount: &str,
) {
    apply(store,p,ledger,&format!("opening-{account}"),json!([{"op":"set_opening","account_id":account,"money":{"currency":currency,"amount_minor":amount},"as_of":"2025-12-31","reason":"Confirmed balance"}])).await.unwrap();
}
async fn create(store: &Store, p: &Principal, ledger: Uuid, key: &str, data: Value) -> Value {
    apply(
        store,
        p,
        ledger,
        key,
        json!([{"op":"create","transaction":data}]),
    )
    .await
    .unwrap()
}
fn created_id(response: &Value) -> Uuid {
    id(&response["data"]["transactions"][0]["transaction_id"])
}
fn current(response: &Value) -> Value {
    response["data"]["current"]["transaction"].clone()
}
async fn revise(
    store: &Store,
    p: &Principal,
    ledger: Uuid,
    key: &str,
    transaction: Uuid,
    data: Value,
) -> Value {
    let version =
        journal(store, p, ledger, transaction).await["data"]["current"]["version"].clone();
    apply(store,p,ledger,key,json!([{"op":"revise","transaction_id":transaction,"expected_version":version,"reason":"Verified correction","transaction":data}])).await.unwrap()
}
async fn linked(
    store: &Store,
    p: &Principal,
    ledger: Uuid,
    key: &str,
    transaction: Uuid,
    refs: Value,
) -> Value {
    let version =
        journal(store, p, ledger, transaction).await["data"]["current"]["version"].clone();
    apply(store,p,ledger,key,json!([{"op":"link_evidence","transaction_id":transaction,"expected_version":version,"reason":"Additional source","evidence_refs":refs}])).await.unwrap()
}
async fn balances(store: &Store, p: &Principal, ledger: Uuid) -> BTreeMap<Uuid, String> {
    report(store, p, ledger, "account_balances").await["data"]["accounts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| {
            (
                id(&v["account"]["id"]),
                v["balance_minor"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}
async fn source(
    store: &Store,
    p: &Principal,
    ledger: Uuid,
    key: &str,
    external: &str,
    body: Value,
) -> Uuid {
    let result=store.sources_ingest(p,parse(json!({"ledger_id":ledger,"idempotency_key":key,"sources":[{"provider":"bank","source_scope":"personal","external_id":external,"body":body.to_string(),"capture_kind":"original"}]}))).await.unwrap();
    id(&result["data"]["sources"][0]["source_version_id"])
}
async fn observations(
    store: &Store,
    p: &Principal,
    ledger: Uuid,
    key: &str,
    items: Value,
) -> Value {
    store
        .observations_ingest(
            p,
            parse(json!({"ledger_id":ledger,"idempotency_key":key,"observations":items})),
        )
        .await
        .unwrap()
}
fn movement(account: Uuid, currency: &str, amount: &str, direction: &str) -> Value {
    json!({"type":"movement","account_id":account,"money":{"currency":currency,"amount_minor":amount},"direction":direction,"posted_on":"2026-01-31"})
}
fn obs(source: Uuid, locator: &str, payload: Value) -> Value {
    json!({"source_version_id":source,"record_locator":locator,"payload":payload,"reason":"Extracted from archived material"})
}
fn evidence(observation: Uuid, version: i64) -> Value {
    json!({"observation_id":observation,"version":version})
}
fn obs_id(result: &Value, index: usize) -> Uuid {
    id(&result["data"]["observations"][index]["observation"]["id"])
}
async fn income(
    store: &Store,
    p: &Principal,
    ledger: Uuid,
    key: &str,
    account: Uuid,
    currency: &str,
    amount: &str,
) -> Uuid {
    created_id(&create(store,p,ledger,key,tx(json!({"kind":"income","receipt_account_id":account,"money":{"currency":currency,"amount_minor":amount},"posted_on":"2026-01-31"}))).await)
}
fn transfer(from: Uuid, to: Uuid, currency: &str, amount: &str) -> Value {
    tx(
        json!({"kind":"transfer","payment_account_id":from,"receipt_account_id":to,"from_money":{"currency":currency,"amount_minor":amount},"to_money":{"currency":currency,"amount_minor":amount},"payment_posted_on":"2026-01-31","receipt_posted_on":"2026-01-31"}),
    )
}

pub async fn acceptance(store: &Store, admin: &PgPool) {
    mcp_import::acceptance(store).await;
    daily_and_refunds(store).await;
    observation_review(store).await;
    fx_and_rounding(store).await;
    reconciliation_matrix(store).await;
    export_recovery(store, admin).await;
    edges::acceptance(store, admin).await;
}
async fn daily_and_refunds(store: &Store) {
    let (p, ledger) = user(store, "daily").await;
    let bank = account(store, &p, ledger, "bank", "CNY").await;
    let wallet = account(store, &p, ledger, "wallet", "CNY").await;
    let cc=id(&store.accounts_upsert(&p,parse(json!({"ledger_id":ledger,"idempotency_key":"cc","name":"Credit card","kind":"liability","currency":"CNY"}))).await.unwrap()["data"]["account"]["id"]);
    opening(store, &p, ledger, bank, "CNY", "100000").await;
    opening(store, &p, ledger, wallet, "CNY", "0").await;
    opening(store, &p, ledger, cc, "CNY", "0").await;
    income(store, &p, ledger, "salary", bank, "CNY", "50000").await;
    let bank_expense =
        created_id(&create(store, &p, ledger, "bank-expense", expense(bank, "10000")).await);
    let cc_expense =
        created_id(&create(store, &p, ledger, "cc-expense", expense(cc, "20000")).await);
    create(
        store,
        &p,
        ledger,
        "wallet-transfer",
        transfer(bank, wallet, "CNY", "5000"),
    )
    .await;
    create(store,&p,ledger,"cc-refund",tx(json!({"kind":"refund","original_transaction_id":cc_expense,"receipt_account_id":cc,"money":{"currency":"CNY","amount_minor":"3000"},"original_money":{"currency":"CNY","amount_minor":"3000"},"refund_allocations":[],"posted_on":"2026-01-31"}))).await;
    create(
        store,
        &p,
        ledger,
        "repay",
        transfer(bank, cc, "CNY", "17000"),
    )
    .await;
    let actual = balances(store, &p, ledger).await;
    assert_eq!(actual[&bank], "118000");
    assert_eq!(actual[&wallet], "5000");
    assert_eq!(actual[&cc], "0");
    let totals = report(store, &p, ledger, "income_expense").await["data"]["totals"][0].clone();
    assert_eq!(totals["income_minor"], "50000");
    assert_eq!(totals["gross_expense_minor"], "30000");
    assert_eq!(totals["refund_minor"], "3000");
    assert_eq!(totals["net_expense_minor"], "27000");
    let preferences=store.preferences_update(&p,parse(json!({"ledger_id":ledger,"idempotency_key":"categories","expected_version":1,"reason":"Personal categories","categories":[{"name":"Food","kind":"expense"},{"name":"Daily","kind":"expense"},{"name":"Books","kind":"expense"}],"tags":[{"name":"Personal"}]}))).await.unwrap();
    let cats = preferences["data"]["preferences"]["categories"]
        .as_array()
        .unwrap();
    let cat = |name: &str| id(&cats.iter().find(|c| c["name"] == name).unwrap()["id"]);
    let food = cat("Food");
    let daily = cat("Daily");
    let books = cat("Books");
    let a = Uuid::from_u128(1);
    let b = Uuid::from_u128(2);
    let mut data = current(&journal(store, &p, ledger, bank_expense).await);
    data["allocations"] = json!([{"allocation_id":a,"category_id":food,"amount_minor":"7000"},{"allocation_id":b,"category_id":daily,"amount_minor":"3000"}]);
    revise(store, &p, ledger, "split", bank_expense, data).await;
    let refund_data = tx(
        json!({"kind":"refund","original_transaction_id":bank_expense,"receipt_account_id":bank,"money":{"currency":"CNY","amount_minor":"2000"},"original_money":{"currency":"CNY","amount_minor":"2000"},"refund_allocations":[{"allocation_id":b,"amount_minor":"2000"}],"posted_on":"2026-01-31"}),
    );
    let refund = created_id(&create(store, &p, ledger, "refund-b", refund_data.clone()).await);
    let mut excess = refund_data.clone();
    excess["movement"]["money"]["amount_minor"] = json!("1500");
    excess["movement"]["original_money"]["amount_minor"] = json!("1500");
    excess["movement"]["refund_allocations"][0]["amount_minor"] = json!("1500");
    assert!(matches!(
        apply(
            store,
            &p,
            ledger,
            "excess-refund",
            json!([{"op":"create","transaction":excess}])
        )
        .await,
        Err(Error::Rule {
            code: "REFUND_OVERALLOCATED",
            ..
        })
    ));
    let mut reclass = current(&journal(store, &p, ledger, bank_expense).await);
    reclass["allocations"][1]["category_id"] = json!(books);
    let personal = id(&preferences["data"]["preferences"]["tags"][0]["id"]);
    reclass["tags"] = json!([personal]);
    revise(
        store,
        &p,
        ledger,
        "reclassify",
        bank_expense,
        reclass.clone(),
    )
    .await;
    let quality = store
        .reports_get(
            &p,
            parse(json!({"ledger_id":ledger,"report":"data_quality","category_id":books})),
        )
        .await
        .unwrap();
    assert_eq!(quality["data"]["transactions"].as_array().unwrap().len(), 2);
    let tagged = store
        .reports_get(
            &p,
            parse(json!({"ledger_id":ledger,"report":"income_expense","tag_id":personal})),
        )
        .await
        .unwrap();
    assert_eq!(tagged["data"]["totals"][0]["net_expense_minor"], "8000");
    let found = store
        .transactions_search(&p, parse(json!({"ledger_id":ledger,"tag_id":personal})))
        .await
        .unwrap();
    assert_eq!(found["data"]["items"].as_array().unwrap().len(), 2);
    let detail = journal(store, &p, ledger, bank_expense).await;
    assert_eq!(detail["data"]["journal"].as_array().unwrap().len(), 1);
    let remaining = detail["data"]["refund_relationships"]["allocations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["allocation_id"] == json!(b))
        .unwrap();
    assert_eq!(remaining["remaining_minor"], "1000");
    let categories = report(store, &p, ledger, "by_category").await;
    let book = categories["data"]["by_category"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["category_id"] == json!(books))
        .unwrap();
    assert_eq!(book["totals"]["gross_expense_minor"], "3000");
    assert_eq!(book["totals"]["refund_minor"], "2000");
    assert_eq!(book["totals"]["net_expense_minor"], "1000");
    let version = detail["data"]["current"]["version"].clone();
    let mut reduced = reclass.clone();
    reduced["allocations"][0]["amount_minor"] = json!("9000");
    reduced["allocations"][1]["amount_minor"] = json!("1000");
    assert!(apply(store,&p,ledger,"reduce-refunded",json!([{"op":"revise","transaction_id":bank_expense,"expected_version":version,"reason":"bad edit","transaction":reduced}])).await.is_err());
    assert!(apply(store,&p,ledger,"void-original",json!([{"op":"void","transaction_id":bank_expense,"expected_version":version,"reason":"bad void"}])).await.is_err());
    let refund_version =
        journal(store, &p, ledger, refund).await["data"]["current"]["version"].clone();
    apply(store,&p,ledger,"void-pair",json!([{"op":"void","transaction_id":bank_expense,"expected_version":version,"reason":"Remove original and related refund together"},{"op":"void","transaction_id":refund,"expected_version":refund_version,"reason":"Remove related refund"}])).await.unwrap();
    let pref = store.context(&p).await.unwrap()["data"]["preferences"].clone();
    let result=store.preferences_update(&p,parse(json!({"ledger_id":ledger,"idempotency_key":"aliases","expected_version":pref["version"],"reason":"Default categorization","merchant_aliases":{"Raw Store":"Shop"},"classification_rules":[{"merchant_contains":"Shop","kind":"expense","category_id":food}]}))).await.unwrap();
    let mut auto = expense(bank, "100");
    auto["merchant"] = json!("Raw Store");
    let auto_id = created_id(&create(store, &p, ledger, "auto-category", auto).await);
    let auto = current(&journal(store, &p, ledger, auto_id).await);
    assert_eq!(auto["normalized_merchant"], "Shop");
    assert_eq!(auto["allocations"][0]["category_id"], json!(food));
    let frozen=store.preferences_update(&p,parse(json!({"ledger_id":ledger,"idempotency_key":"timezone","expected_version":result["data"]["preferences"]["version"],"reason":"Change timezone","timezone":"UTC"}))).await;
    assert!(frozen.is_err());
}

async fn observation_review(store: &Store) {
    let (p, ledger) = user(store, "evidence").await;
    let bank = account(store, &p, ledger, "bank", "CNY").await;
    opening(store, &p, ledger, bank, "CNY", "100000").await;
    let src = source(
        store,
        &p,
        ledger,
        "source",
        "payment",
        json!({"records":[{"locator":"row1"}],"amount":"10000"}),
    )
    .await;
    let row = observations(
        store,
        &p,
        ledger,
        "row",
        json!([obs(src, "row1", movement(bank, "CNY", "10000", "out"))]),
    )
    .await;
    let observation = obs_id(&row, 0);
    let mut data = expense(bank, "10000");
    data["evidence_refs"] = json!([evidence(observation, 1)]);
    let transaction = created_id(&create(store, &p, ledger, "purchase", data).await);
    let detail = journal(store, &p, ledger, transaction).await;
    assert!(
        !detail["data"]["quality_flags"]
            .as_array()
            .unwrap()
            .contains(&json!("evidence_missing"))
    );
    let change=observations(store,&p,ledger,"correct-observation",json!([{"observation_id":observation,"expected_version":1,"payload":movement(bank,"CNY","1000","out"),"reason":"Original extraction had an extra zero"}])).await;
    assert_eq!(
        change["data"]["observations"][0]["status"]["occupied_minor"],
        "10000"
    );
    assert_eq!(
        change["data"]["observations"][0]["status"]["overallocated"],
        true
    );
    assert_eq!(balances(store, &p, ledger).await[&bank], "90000");
    assert_eq!(
        report(store, &p, ledger, "income_expense").await["data"]["totals"][0]["gross_expense_minor"],
        "10000"
    );
    let mut duplicate = expense(bank, "1000");
    duplicate["evidence_refs"] = json!([evidence(observation, 2)]);
    assert!(matches!(
        apply(
            store,
            &p,
            ledger,
            "cannot-bypass",
            json!([{"op":"create","transaction":duplicate}])
        )
        .await,
        Err(Error::Rule {
            code: "EVIDENCE_REVIEW_REQUIRED",
            ..
        })
    ));
    let fixed = revise(store, &p, ledger, "fix-transaction", transaction, duplicate).await;
    assert_eq!(fixed["data"]["transactions"][0]["version"], 2);
    assert_eq!(balances(store, &p, ledger).await[&bank], "99000");
    let note=observations(store,&p,ledger,"observation-note",json!([{"observation_id":observation,"expected_version":2,"review_note":"Reviewed original amount","reason":"Save review"}])).await;
    assert_eq!(note["data"]["observations"][0]["observation"]["version"], 3);
    assert_eq!(
        note["data"]["observations"][0]["observation"]["current_version"],
        2
    );
    assert_eq!(
        note["data"]["observations"][0]["status"]["needs_review"],
        false
    );
    let noop=observations(store,&p,ledger,"note-again",json!([{"observation_id":observation,"expected_version":3,"review_note":"Reviewed original amount","reason":"A different request reason"}])).await;
    assert_eq!(noop["changed"], false);
    assert_eq!(noop["ledger_revision"], note["ledger_revision"]);
    assert!(store.observations_ingest(&p,parse(json!({"ledger_id":ledger,"idempotency_key":"ignore-movement","observations":[{"observation_id":observation,"expected_version":3,"ignored_reason":"skip","reason":"bad skip"}]}))).await.is_err());
    apply(store,&p,ledger,"unlink",json!([{"op":"unlink_evidence","transaction_id":transaction,"expected_version":2,"reason":"Keep money, remove evidence","observation_ids":[observation]}])).await.unwrap();
    assert!(
        journal(store, &p, ledger, transaction).await["data"]["quality_flags"]
            .as_array()
            .unwrap()
            .contains(&json!("evidence_missing"))
    );
    linked(
        store,
        &p,
        ledger,
        "relink",
        transaction,
        json!([evidence(observation, 2)]),
    )
    .await;
    let src2 = source(
        store,
        &p,
        ledger,
        "source-version2",
        "payment",
        json!({"records":[{"locator":"row1"}],"amount":"1000","updated":true}),
    )
    .await;
    assert!(store.observations_ingest(&p,parse(json!({"ledger_id":ledger,"idempotency_key":"new-id-for-same-row","observations":[obs(src2,"row1",movement(bank,"CNY","1000","out"))]}))).await.is_err());
    observations(store,&p,ledger,"map-stable-row",json!([{"observation_id":observation,"expected_version":3,"source_version_id":src2,"record_locator":"row1","payload":movement(bank,"CNY","1000","out"),"reason":"Same row in corrected source"}])).await;
    linked(
        store,
        &p,
        ledger,
        "rebind-source-version",
        transaction,
        json!([evidence(observation, 3)]),
    )
    .await;
    let source_again = source(
        store,
        &p,
        ledger,
        "same-source-again",
        "payment",
        json!({"records":[{"locator":"row1"}],"amount":"1000","updated":true}),
    )
    .await;
    assert_eq!(source_again, src2);
    let independent = source(
        store,
        &p,
        ledger,
        "other-source",
        "notification",
        json!({"message":"paid 10"}),
    )
    .await;
    let other = observations(
        store,
        &p,
        ledger,
        "other-row",
        json!([obs(
            independent,
            "line:1",
            movement(bank, "CNY", "1000", "out")
        )]),
    )
    .await;
    let other_id = obs_id(&other, 0);
    let mut identified = current(&journal(store, &p, ledger, transaction).await);
    identified["identifiers"] = json!([{"namespace":"bank-settlement","account_id":bank,"event_type":"payment","external_id":"payment-123"}]);
    revise(
        store,
        &p,
        ledger,
        "strong-id",
        transaction,
        identified.clone(),
    )
    .await;
    let mut imported = expense(bank, "1000");
    imported["identifiers"] = identified["identifiers"].clone();
    imported["evidence_refs"] = json!([evidence(other_id, 1)]);
    let dedup = create(store, &p, ledger, "same-strong-id", imported.clone()).await;
    assert_eq!(created_id(&dedup), transaction);
    assert_eq!(dedup["data"]["transactions"][0]["deduplicated"], true);
    let dedup_noop = create(store, &p, ledger, "same-strong-id-again", imported.clone()).await;
    assert_eq!(dedup_noop["changed"], false);
    assert_eq!(dedup_noop["ledger_revision"], dedup["ledger_revision"]);
    imported["movement"]["money"]["amount_minor"] = json!("2000");
    assert!(matches!(
        apply(
            store,
            &p,
            ledger,
            "conflicting-strong-id",
            json!([{"op":"create","transaction":imported}])
        )
        .await,
        Err(Error::Rule {
            code: "STRONG_IDENTIFIER_CONFLICT",
            ..
        })
    ));
    assert_eq!(
        report(store, &p, ledger, "income_expense").await["data"]["totals"][0]["gross_expense_minor"],
        "1000"
    );
    // One stable movement may be split, but not across versions to bypass occupancy.
    let split_source = source(
        store,
        &p,
        ledger,
        "split-source",
        "split",
        json!({"amount":"1000"}),
    )
    .await;
    let split_rows = observations(
        store,
        &p,
        ledger,
        "split-row",
        json!([obs(
            split_source,
            "row",
            movement(bank, "CNY", "1000", "out")
        )]),
    )
    .await;
    let split = obs_id(&split_rows, 0);
    let mut left = expense(bank, "600");
    left["evidence_refs"] = json!([{"observation_id":split,"version":1,"amount_minor":"600"}]);
    let left_id = created_id(&create(store, &p, ledger, "split-left", left).await);
    let mut right = expense(bank, "400");
    right["evidence_refs"] = json!([{"observation_id":split,"version":1,"amount_minor":"400"}]);
    let right_id = created_id(&create(store, &p, ledger, "split-right", right).await);
    observations(store,&p,ledger,"split-shrink",json!([{"observation_id":split,"expected_version":1,"payload":movement(bank,"CNY","500","out"),"reason":"Corrected row"}])).await;
    let mut left = expense(bank, "300");
    left["evidence_refs"] = json!([{"observation_id":split,"version":2,"amount_minor":"300"}]);
    let mut right = expense(bank, "200");
    right["evidence_refs"] = json!([{"observation_id":split,"version":2,"amount_minor":"200"}]);
    apply(store,&p,ledger,"split-atomic-review",json!([{"op":"revise","transaction_id":left_id,"expected_version":1,"reason":"Review both allocations","transaction":left},{"op":"revise","transaction_id":right_id,"expected_version":1,"reason":"Review both allocations","transaction":right}])).await.unwrap();
}

async fn fx_and_rounding(store: &Store) {
    let (p, ledger) = user(store, "fx").await;
    let cny = account(store, &p, ledger, "cny", "CNY").await;
    let usd = account(store, &p, ledger, "usd", "USD").await;
    opening(store, &p, ledger, cny, "CNY", "100000").await;
    opening(store, &p, ledger, usd, "USD", "0").await;
    let mut exchange = tx(
        json!({"kind":"transfer","payment_account_id":cny,"receipt_account_id":usd,"from_money":{"currency":"CNY","amount_minor":"72000"},"to_money":{"currency":"USD","amount_minor":"10000"},"payment_posted_on":"2026-01-31","receipt_posted_on":"2026-01-31"}),
    );
    exchange["fees"] = json!([{"payment_account_id":cny,"money":{"currency":"CNY","amount_minor":"1000"},"posted_on":"2026-01-31"}]);
    create(store, &p, ledger, "exchange", exchange).await;
    let purchase=created_id(&create(store,&p,ledger,"usd-purchase",tx(json!({"kind":"expense","payment_account_id":usd,"money":{"currency":"USD","amount_minor":"2000"},"posted_on":"2026-01-31"}))).await);
    let b = balances(store, &p, ledger).await;
    assert_eq!(b[&cny], "27000");
    assert_eq!(b[&usd], "8000");
    let request =
        || parse(json!({"ledger_id":ledger,"report":"income_expense","display_currency":"CNY"}));
    let missing = store.reports_get(&p, request()).await.unwrap();
    assert_eq!(missing["data"]["display_totals"]["complete"], false);
    let src = source(
        store,
        &p,
        ledger,
        "quote-source",
        "quotes",
        json!({"USD/CNY":"7.30","JPY/USD":"0.006"}),
    )
    .await;
    let archived = store
        .sources_get(
            &p,
            parse(json!({"ledger_id":ledger,"source_version_id":src,"record_locator":"/USD~1CNY"})),
        )
        .await
        .unwrap();
    assert_eq!(archived["data"]["excerpt"], "7.30");
    store.fx_rates_ingest(&p,parse(json!({"ledger_id":ledger,"idempotency_key":"quotes","rates":[{"from_currency":"USD","to_currency":"CNY","on":"2026-01-30","rate":"7.30","source_version_id":src,"record_locator":"/USD~1CNY"},{"from_currency":"JPY","to_currency":"USD","on":"2026-01-30","rate":"0.006","source_version_id":src,"record_locator":"/JPY~1USD"}]}))).await.unwrap();
    let converted = store.reports_get(&p, request()).await.unwrap();
    assert_eq!(converted["data"]["display_totals"]["complete"], true);
    assert_eq!(
        converted["data"]["display_totals"]["totals"]["gross_expense_minor"],
        "15600"
    );
    let allocation =
        journal(store, &p, ledger, purchase).await["data"]["current"]["transaction"]["allocations"]
            [0]["allocation_id"]
            .clone();
    create(store,&p,ledger,"cross-currency-refund",tx(json!({"kind":"refund","original_transaction_id":purchase,"receipt_account_id":cny,"money":{"currency":"CNY","amount_minor":"3600"},"original_money":{"currency":"USD","amount_minor":"500"},"refund_allocations":[{"allocation_id":allocation,"amount_minor":"500"}],"posted_on":"2026-01-31"}))).await;
    let refunded = store.reports_get(&p, request()).await.unwrap();
    assert_eq!(
        refunded["data"]["display_totals"]["totals"]["refund_minor"],
        "3600"
    );
    let jpy = account(store, &p, ledger, "jpy", "JPY").await;
    opening(store, &p, ledger, jpy, "JPY", "10").await;
    let prefs=store.preferences_update(&p,parse(json!({"ledger_id":ledger,"idempotency_key":"rounding-categories","expected_version":1,"reason":"Rounding categories","categories":[{"name":"a","kind":"expense"},{"name":"b","kind":"expense"},{"name":"c","kind":"expense"},{"name":"d","kind":"expense"}]}))).await.unwrap();
    let cats = prefs["data"]["preferences"]["categories"]
        .as_array()
        .unwrap();
    let mut allocations = vec![];
    let mut d_category = None;
    for (i, name) in ["a", "b", "c", "d"].iter().enumerate() {
        let category = id(&cats.iter().find(|c| c["name"] == *name).unwrap()["id"]);
        if *name == "d" {
            d_category = Some(category);
        }
        allocations.push(json!({"allocation_id":Uuid::from_u128((i+1)as u128),"category_id":category,"amount_minor":"1"}));
    }
    let mut tiny = tx(
        json!({"kind":"expense","payment_account_id":jpy,"money":{"currency":"JPY","amount_minor":"4"},"posted_on":"2026-01-31"}),
    );
    tiny["allocations"] = json!(allocations);
    let tiny_id = created_id(&create(store, &p, ledger, "tiny-purchase", tiny).await);
    let tiny_report=store.reports_get(&p,parse(json!({"ledger_id":ledger,"report":"by_category","currency":"JPY","display_currency":"USD"}))).await.unwrap();
    assert_eq!(
        tiny_report["data"]["display_totals"]["totals"]["gross_expense_minor"],
        "2"
    );
    let portions = &tiny_report["data"]["components"][0]["display_allocations"];
    assert_eq!(portions[Uuid::from_u128(1).to_string()], "1");
    assert_eq!(portions[Uuid::from_u128(2).to_string()], "1");
    assert_eq!(portions[Uuid::from_u128(3).to_string()], "0");
    assert_eq!(portions[Uuid::from_u128(4).to_string()], "0");
    let filtered=store.reports_get(&p,parse(json!({"ledger_id":ledger,"report":"by_category","currency":"JPY","category_id":d_category,"display_currency":"USD"}))).await.unwrap();
    assert_eq!(
        filtered["data"]["display_totals"]["totals"]["gross_expense_minor"],
        "0"
    );
    let mut reordered = current(&journal(store, &p, ledger, tiny_id).await);
    reordered["allocations"].as_array_mut().unwrap().reverse();
    revise(store, &p, ledger, "reorder-allocations", tiny_id, reordered).await;
    let again=store.reports_get(&p,parse(json!({"ledger_id":ledger,"report":"by_category","currency":"JPY","display_currency":"USD"}))).await.unwrap();
    assert_eq!(
        again["data"]["components"][0]["display_allocations"],
        *portions
    );
    let refund_allocations = (1..=4)
        .map(|i| json!({"allocation_id":Uuid::from_u128(i),"amount_minor":"1"}))
        .collect::<Vec<_>>();
    create(store,&p,ledger,"refund-four-allocations",tx(json!({"kind":"refund","original_transaction_id":tiny_id,"receipt_account_id":usd,"money":{"currency":"USD","amount_minor":"2"},"original_money":{"currency":"JPY","amount_minor":"4"},"refund_allocations":refund_allocations,"posted_on":"2026-01-31"}))).await;
    let refunded=store.reports_get(&p,parse(json!({"ledger_id":ledger,"report":"by_category","currency":"JPY","display_currency":"USD"}))).await.unwrap();
    assert_eq!(
        refunded["data"]["display_totals"]["totals"]["refund_minor"],
        "2"
    );
    assert_eq!(
        refunded["data"]["display_totals"]["totals"]["net_expense_minor"],
        "0"
    );
    for component in refunded["data"]["components"].as_array().unwrap() {
        assert_eq!(component["display_allocations"], *portions);
    }
}

fn statement(account: Uuid) -> Value {
    json!({"type":"statement","account_id":account,"currency":"CNY","period_start":"2026-01-01","period_end":"2026-02-01","opening_balance":"100000","closing_balance":"100000","expected_count":{"value":2,"locator":"/count"},"expected_debit_total":{"value":"10000","locator":"/debit"},"expected_credit_total":{"value":"10000","locator":"/credit"},"source_coverage":"complete","record_locators":["d","c"],"range_locator":"/records","extraction_complete":true})
}
async fn check(store: &Store, p: &Principal, ledger: Uuid, bank: Uuid, statement: Uuid) -> Value {
    let rows = store
        .observations_search(
            p,
            parse(json!({"ledger_id":ledger,"observation_type":"statement"})),
        )
        .await
        .unwrap();
    let version = rows["data"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["observation"]["id"] == json!(statement))
        .unwrap()["observation"]["current_version"]
        .clone();
    store.reconcile(p,parse(json!({"ledger_id":ledger,"account_id":bank,"currency":"CNY","period_start":"2026-01-01","period_end":"2026-02-01","statement_observation_id":statement,"statement_version":version}))).await.unwrap()
}
async fn reconciliation_matrix(store: &Store) {
    let (p, ledger) = user(store, "reconcile").await;
    let bank = account(store, &p, ledger, "bank", "CNY").await;
    opening(store, &p, ledger, bank, "CNY", "100000").await;
    let src=source(store,&p,ledger,"statement-source","statement",json!({"records":[{"locator":"d"},{"locator":"c"}],"count":2,"debit":"10000","credit":"10000"})).await;
    let mut data = statement(bank);
    data["extraction_complete"] = json!(false);
    let saved = observations(
        store,
        &p,
        ledger,
        "statement-only",
        json!([obs(src, "statement", data)]),
    )
    .await;
    let stmt = obs_id(&saved, 0);
    let balance_only = check(store, &p, ledger, bank, stmt).await;
    assert_eq!(
        balance_only["data"]["reconciliation"]["status"],
        "balance_matched"
    );
    assert_eq!(
        balance_only["data"]["import_completeness"]["status"],
        "incomplete"
    );
    let rows=observations(store,&p,ledger,"all-movements",json!([obs(src,"d",movement(bank,"CNY","10000","out")),obs(src,"c",movement(bank,"CNY","10000","in")),{"observation_id":stmt,"expected_version":1,"payload":statement(bank),"reason":"Finished extraction"}])).await;
    let debit = obs_id(&rows, 0);
    let credit = obs_id(&rows, 1);
    let acquired = check(store, &p, ledger, bank, stmt).await;
    assert_eq!(
        acquired["data"]["reconciliation"]["status"],
        "balance_matched"
    );
    assert_eq!(
        acquired["data"]["import_completeness"]["status"],
        "complete"
    );
    let mut out = expense(bank, "10000");
    out["evidence_refs"] = json!([evidence(debit, 1)]);
    let out_id = created_id(&create(store, &p, ledger, "post-debit", out).await);
    let mut incoming = tx(
        json!({"kind":"income","receipt_account_id":bank,"money":{"currency":"CNY","amount_minor":"10000"},"posted_on":"2026-01-31"}),
    );
    incoming["evidence_refs"] = json!([evidence(credit, 1)]);
    let in_id = created_id(&create(store, &p, ledger, "post-credit", incoming).await);
    let complete = check(store, &p, ledger, bank, stmt).await;
    assert_eq!(complete["data"]["reconciliation"]["status"], "reconciled");
    assert_eq!(
        complete["data"]["import_completeness"]["status"],
        "complete"
    );
    let extra_out =
        created_id(&create(store, &p, ledger, "extra-out", expense(bank, "5000")).await);
    let extra_in = income(store, &p, ledger, "extra-in", bank, "CNY", "5000").await;
    let extras = check(store, &p, ledger, bank, stmt).await;
    assert_eq!(
        extras["data"]["reconciliation"]["status"],
        "balance_matched"
    );
    assert_eq!(extras["data"]["import_completeness"]["status"], "complete");
    apply(store,&p,ledger,"void-extra-pair",json!([{"op":"void","transaction_id":extra_out,"expected_version":1,"reason":"Duplicate"},{"op":"void","transaction_id":extra_in,"expected_version":1,"reason":"Duplicate"}])).await.unwrap();
    // No control values: full manifests can prove completeness, otherwise it remains unknown.
    let src2 = source(
        store,
        &p,
        ledger,
        "no-controls-source",
        "no-controls",
        json!({"records":[{"locator":"d"},{"locator":"c"}]}),
    )
    .await;
    let mut no_controls = statement(bank);
    for field in [
        "expected_count",
        "expected_debit_total",
        "expected_credit_total",
        "record_locators",
        "range_locator",
    ] {
        no_controls[field] = Value::Null;
    }
    no_controls["source_coverage"] = json!("unknown");
    let imported = observations(
        store,
        &p,
        ledger,
        "no-controls-rows",
        json!([
            obs(src2, "statement", no_controls),
            obs(src2, "d", movement(bank, "CNY", "10000", "out")),
            obs(src2, "c", movement(bank, "CNY", "10000", "in"))
        ]),
    )
    .await;
    let stmt2 = obs_id(&imported, 0);
    linked(
        store,
        &p,
        ledger,
        "link-debit-other-source",
        out_id,
        json!([evidence(obs_id(&imported, 1), 1)]),
    )
    .await;
    linked(
        store,
        &p,
        ledger,
        "link-credit-other-source",
        in_id,
        json!([evidence(obs_id(&imported, 2), 1)]),
    )
    .await;
    let unknown = check(store, &p, ledger, bank, stmt2).await;
    assert_eq!(unknown["data"]["reconciliation"]["status"], "reconciled");
    assert_eq!(unknown["data"]["import_completeness"]["status"], "unknown");
    assert_eq!(
        unknown["data"]["import_completeness"]["controls"]["count"]["status"],
        "unavailable"
    );
    let mut manifested = statement(bank);
    for field in [
        "expected_count",
        "expected_debit_total",
        "expected_credit_total",
    ] {
        manifested[field] = Value::Null;
    }
    observations(store,&p,ledger,"manifest-without-controls",json!([{"observation_id":stmt2,"expected_version":1,"payload":manifested,"reason":"Structured record range verified"}])).await;
    let verified = check(store, &p, ledger, bank, stmt2).await;
    assert_eq!(verified["data"]["reconciliation"]["status"], "reconciled");
    assert_eq!(
        verified["data"]["import_completeness"]["status"],
        "complete"
    );
    let mut bad = statement(bank);
    bad["expected_count"]["value"] = json!(3);
    observations(store,&p,ledger,"bad-control",json!([{"observation_id":stmt,"expected_version":2,"payload":bad,"reason":"Source claims a third row"}])).await;
    let mismatch = check(store, &p, ledger, bank, stmt).await;
    assert_eq!(mismatch["data"]["reconciliation"]["status"], "reconciled");
    assert_eq!(
        mismatch["data"]["import_completeness"]["status"],
        "incomplete"
    );
    let mut omit = statement(bank);
    for field in [
        "expected_count",
        "expected_debit_total",
        "expected_credit_total",
    ] {
        omit[field] = Value::Null;
    }
    let retained=observations(store,&p,ledger,"omit-known-control",json!([{"observation_id":stmt,"expected_version":3,"payload":omit,"reason":"Ordinary reimport"}])).await;
    assert_eq!(
        retained["data"]["observations"][0]["observation"]["payload"]["expected_count"]["value"],
        3
    );
    let unknown_bank = account(store, &p, ledger, "unknown-opening", "CNY").await;
    let empty_source = source(
        store,
        &p,
        ledger,
        "empty-source",
        "empty",
        json!({"records":[]}),
    )
    .await;
    let mut empty = statement(unknown_bank);
    empty["opening_balance"] = json!("0");
    empty["closing_balance"] = json!("0");
    empty["record_locators"] = json!([]);
    for field in [
        "expected_count",
        "expected_debit_total",
        "expected_credit_total",
    ] {
        empty[field] = Value::Null;
    }
    let empty_obs = observations(
        store,
        &p,
        ledger,
        "empty-statement",
        json!([obs(empty_source, "statement", empty)]),
    )
    .await;
    let empty_id = obs_id(&empty_obs, 0);
    let unknown_opening = check(store, &p, ledger, unknown_bank, empty_id).await;
    assert_eq!(
        unknown_opening["data"]["reconciliation"]["status"],
        "incomplete"
    );
    assert_eq!(
        unknown_opening["data"]["import_completeness"]["status"],
        "complete"
    );
    opening(store, &p, ledger, unknown_bank, "CNY", "0").await;
    let confirmed_empty = check(store, &p, ledger, unknown_bank, empty_id).await;
    assert_eq!(
        confirmed_empty["data"]["reconciliation"]["status"],
        "reconciled"
    );
    assert_eq!(
        confirmed_empty["data"]["import_completeness"]["status"],
        "complete"
    );
}

async fn export_recovery(store: &Store, admin: &PgPool) {
    let (p, ledger) = user(store, "evidence").await;
    let export = report(store, &p, ledger, "export").await["data"].clone();
    assert_eq!(export["format"], "sumi-export-v1");
    assert!(
        export["tables"]["journal_lines"]
            .as_array()
            .unwrap()
            .iter()
            .all(|l| l["amount_minor"].is_string())
    );
    let name = format!("sumi_restore_{}", Uuid::new_v4().simple());
    sqlx::query(&format!("CREATE DATABASE {name}"))
        .execute(admin)
        .await
        .unwrap();
    let target = PgPool::connect_with(admin.connect_options().as_ref().clone().database(&name))
        .await
        .unwrap();
    sqlx::migrate!().run(&target).await.unwrap();
    sumi::store::restore_export(&target, export.clone())
        .await
        .unwrap();
    assert!(
        sumi::store::restore_export(&target, export.clone())
            .await
            .is_err()
    );
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
    assert_eq!(
        report(&restored, &p, ledger, "income_expense").await["data"],
        report(store, &p, ledger, "income_expense").await["data"]
    );
    assert_eq!(
        report(&restored, &p, ledger, "account_balances").await["data"],
        report(store, &p, ledger, "account_balances").await["data"]
    );
    assert_eq!(
        report(&restored, &p, ledger, "export").await["data"]["content_hash"],
        export["content_hash"]
    );
    restored_pool.close().await;
    target.close().await;
    sqlx::query(&format!("DROP DATABASE {name} WITH (FORCE)"))
        .execute(admin)
        .await
        .unwrap();
}
