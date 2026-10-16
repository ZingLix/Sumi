use super::*;

#[derive(Clone, Serialize)]
pub(super) struct Component {
    pub transaction_id: Uuid,
    pub component_id: Uuid,
    pub kind: String,
    pub money: Money,
    pub occurred_on: Option<NaiveDate>,
    pub allocations: Vec<Allocation>,
    pub account_ids: Vec<Uuid>,
    pub merchant: Option<String>,
    pub normalized_merchant: Option<String>,
    pub tags: Vec<Uuid>,
    pub settlement: Option<Settlement>,
    pub actual_refund_receipt: Option<Money>,
    pub quality_flags: Vec<String>,
}
#[derive(Default, Clone, Serialize)]
struct Totals {
    income_minor: Amount,
    gross_expense_minor: Amount,
    refund_minor: Amount,
    net_expense_minor: Amount,
    uncategorized_income_minor: Amount,
    uncategorized_expense_minor: Amount,
}
impl Totals {
    fn add(&mut self, kind: &str, amount: Amount, uncategorized: bool) -> Result<()> {
        match kind {
            "income" => {
                self.income_minor = self.income_minor.checked_add(amount)?;
                if uncategorized {
                    self.uncategorized_income_minor =
                        self.uncategorized_income_minor.checked_add(amount)?;
                }
            }
            "refund" => {
                self.refund_minor = self.refund_minor.checked_add(amount)?;
                self.net_expense_minor =
                    self.net_expense_minor.checked_add(amount.checked_neg()?)?;
                if uncategorized {
                    self.uncategorized_expense_minor = self
                        .uncategorized_expense_minor
                        .checked_add(amount.checked_neg()?)?;
                }
            }
            _ => {
                self.gross_expense_minor = self.gross_expense_minor.checked_add(amount)?;
                self.net_expense_minor = self.net_expense_minor.checked_add(amount)?;
                if uncategorized {
                    self.uncategorized_expense_minor =
                        self.uncategorized_expense_minor.checked_add(amount)?;
                }
            }
        }
        Ok(())
    }
}
impl Store {
    pub(super) async fn reports_complete(
        &self,
        p: &Principal,
        request: ReportsGet,
    ) -> Result<Value> {
        require(
            request.from.zip(request.before).is_none_or(|(a, b)| a < b),
            "Date range must be [from,before)",
        )?;
        let mut s = self.session(p, request.ledger_id, false).await?;
        if let Some(currency) = &request.currency {
            currency_precision(currency)?;
        }
        if let Some(currency) = &request.display_currency {
            currency_precision(currency)?;
        }
        let data = match request.report {
            ReportKind::Export => {
                require(
                    request.from.is_none()
                        && request.before.is_none()
                        && request.account_id.is_none()
                        && request.currency.is_none()
                        && request.merchant.is_none()
                        && request.category_id.is_none()
                        && request.tag_id.is_none()
                        && request.display_currency.is_none(),
                    "A full export cannot be filtered",
                )?;
                s.export_snapshot(p).await?
            }
            ReportKind::DataQuality => s.filtered_quality_report(&request).await?,
            ReportKind::AccountBalances => s.balance_report(&request).await?,
            ReportKind::IncomeExpense | ReportKind::ByCategory => {
                s.component_report(&request).await?
            }
        };
        let response = envelope(data, s.revision);
        s.tx.commit().await?;
        Ok(response)
    }
}
impl Session {
    pub(super) async fn components(&mut self) -> Result<Vec<Component>> {
        let transactions = self
            .current_transactions()
            .await?
            .into_iter()
            .map(|t| (t.id, t))
            .collect::<BTreeMap<_, _>>();
        let mut result = vec![];
        for t in transactions.values().filter(|t| t.state == "posted") {
            let data = &t.transaction;
            let quality = self.transaction_quality(t).await?;
            let principal = match &data.movement {
                Movement::Expense {
                    money,
                    payment_account_id,
                    ..
                } => Some((
                    "expense",
                    money.clone(),
                    data.allocations.clone(),
                    payment_account_id.iter().copied().collect(),
                    None,
                )),
                Movement::Income {
                    money,
                    receipt_account_id,
                    ..
                } => Some((
                    "income",
                    money.clone(),
                    data.allocations.clone(),
                    vec![*receipt_account_id],
                    None,
                )),
                Movement::Refund {
                    original_transaction_id,
                    receipt_account_id,
                    money,
                    original_money,
                    refund_allocations,
                    ..
                } => {
                    let original = transactions
                        .get(original_transaction_id)
                        .ok_or(Error::NotFound)?;
                    let allocations = refund_allocations
                        .iter()
                        .map(|r| {
                            let source = original
                                .transaction
                                .allocations
                                .iter()
                                .find(|a| a.allocation_id == Some(r.allocation_id))
                                .ok_or(Error::NotFound)?;
                            Ok(Allocation {
                                allocation_id: Some(r.allocation_id),
                                category_id: source.category_id,
                                amount_minor: r.amount_minor,
                                tags: source.tags.clone(),
                            })
                        })
                        .collect::<Result<Vec<_>>>()?;
                    Some((
                        "refund",
                        original_money.clone(),
                        allocations,
                        vec![*receipt_account_id],
                        Some(money.clone()),
                    ))
                }
                _ => None,
            };
            if let Some((kind, money, allocations, account_ids, actual_refund_receipt)) = principal
            {
                let mut component_tags = data.tags.clone();
                if let Movement::Refund {
                    original_transaction_id,
                    ..
                } = data.movement
                {
                    component_tags.extend(
                        &transactions
                            .get(&original_transaction_id)
                            .ok_or(Error::NotFound)?
                            .transaction
                            .tags,
                    );
                    component_tags.sort();
                    component_tags.dedup();
                }
                result.push(Component {
                    transaction_id: t.id,
                    component_id: t.id,
                    kind: kind.into(),
                    money,
                    occurred_on: data.occurred_on,
                    allocations,
                    account_ids,
                    merchant: data.merchant.clone(),
                    normalized_merchant: data.normalized_merchant.clone(),
                    tags: component_tags,
                    settlement: data.settlement.clone(),
                    actual_refund_receipt,
                    quality_flags: quality.clone(),
                });
            }
            for fee in &data.fees {
                result.push(Component {
                    transaction_id: t.id,
                    component_id: fee.fee_id.ok_or(Error::NotFound)?,
                    kind: "expense".into(),
                    money: fee.money.clone(),
                    occurred_on: fee.occurred_on.or(data.occurred_on),
                    allocations: fee.allocations.clone(),
                    account_ids: vec![fee.payment_account_id],
                    merchant: data.merchant.clone(),
                    normalized_merchant: data.normalized_merchant.clone(),
                    tags: data.tags.clone(),
                    settlement: fee.settlement.clone(),
                    actual_refund_receipt: None,
                    quality_flags: quality.clone(),
                });
            }
        }
        Ok(result)
    }
    pub(super) async fn quote(
        &mut self,
        from: &str,
        to: &str,
        date: NaiveDate,
    ) -> Result<Option<Value>> {
        sqlx::query_scalar("SELECT to_jsonb(r)-'ledger_id' FROM fx_rates r WHERE ledger_id=$1 AND from_currency=$2 AND to_currency=$3 AND on_date=$4 ORDER BY recorded_revision DESC,id DESC LIMIT 1").bind(self.ledger_id).bind(from).bind(to).bind(date).fetch_optional(&mut *self.tx).await.map_err(Into::into)
    }
    pub(super) async fn component_conversion(
        &mut self,
        c: &Component,
        to: &str,
    ) -> Result<Option<(Amount, Value)>> {
        if c.money.currency == to {
            return Ok(Some((
                c.money.amount_minor,
                json!({"basis":"original_currency","currency":to}),
            )));
        }
        if let Some(settlement) = &c.settlement
            && settlement.money.currency == to
        {
            return Ok(Some((
                settlement.money.amount_minor,
                json!({"basis":"actual_settlement","source_version_id":settlement.source_version_id,"record_locator":settlement.record_locator}),
            )));
        }
        if let Some(receipt) = &c.actual_refund_receipt
            && receipt.currency == to
        {
            return Ok(Some((
                receipt.amount_minor,
                json!({"basis":"actual_refund_receipt","transaction_id":c.transaction_id}),
            )));
        }
        let Some(date) = c.occurred_on else {
            return Ok(None);
        };
        let Some(quote) = self.quote(&c.money.currency, to, date).await? else {
            return Ok(None);
        };
        let amount = convert(
            c.money.amount_minor,
            &c.money.currency,
            to,
            quote["rate"].as_str().ok_or(Error::NotFound)?,
        )?;
        Ok(Some((amount, json!({"basis":"dated_quote","quote":quote}))))
    }
    pub(super) async fn component_report(&mut self, r: &ReportsGet) -> Result<Value> {
        let components = self.components().await?;
        let mut totals = BTreeMap::<String, Totals>::new();
        let mut categories = BTreeMap::<(Option<Uuid>, String), Totals>::new();
        let mut display = Totals::default();
        let mut display_categories = BTreeMap::<Option<Uuid>, Totals>::new();
        let mut missing = vec![];
        let mut used_quotes = vec![];
        let mut rows = vec![];
        let mut undated = vec![];
        let mut quality_affected = vec![];
        for c in components {
            if r.account_id.is_some_and(|id| !c.account_ids.contains(&id))
                || r.currency.as_ref().is_some_and(|v| v != &c.money.currency)
                || r.merchant.as_ref().is_some_and(|m| {
                    c.merchant.as_ref() != Some(m) && c.normalized_merchant.as_ref() != Some(m)
                })
            {
                continue;
            }
            let Some(date) = c.occurred_on else {
                undated.push(json!({"transaction_id":c.transaction_id,"component_id":c.component_id,"money":c.money}));
                continue;
            };
            if r.from.is_some_and(|d| date < d) || r.before.is_some_and(|d| date >= d) {
                continue;
            }
            let conversion = if let Some(to) = &r.display_currency {
                self.component_conversion(&c, to).await?
            } else {
                None
            };
            let converted_allocations = if let Some((amount, _)) = &conversion {
                Some(apportion(
                    *amount,
                    &c.allocations
                        .iter()
                        .map(|a| Ok((a.allocation_id.ok_or(Error::NotFound)?, a.amount_minor)))
                        .collect::<Result<Vec<_>>>()?,
                )?)
            } else {
                None
            };
            let selected = c
                .allocations
                .iter()
                .filter(|a| {
                    r.category_id.is_none_or(|id| a.category_id == Some(id))
                        && r.tag_id
                            .is_none_or(|id| c.tags.contains(&id) || a.tags.contains(&id))
                })
                .collect::<Vec<_>>();
            if selected.is_empty() {
                continue;
            }
            let mut selected_amount = Amount::ZERO;
            let mut selected_display = Amount::ZERO;
            for allocation in &selected {
                selected_amount = selected_amount.checked_add(allocation.amount_minor)?;
                totals.entry(c.money.currency.clone()).or_default().add(
                    &c.kind,
                    allocation.amount_minor,
                    allocation.category_id.is_none(),
                )?;
                categories
                    .entry((allocation.category_id, c.money.currency.clone()))
                    .or_default()
                    .add(
                        &c.kind,
                        allocation.amount_minor,
                        allocation.category_id.is_none(),
                    )?;
                if let Some(converted) = &converted_allocations {
                    let amount = *converted
                        .get(&allocation.allocation_id.ok_or(Error::NotFound)?)
                        .ok_or(Error::NotFound)?;
                    selected_display = selected_display.checked_add(amount)?;
                    display.add(&c.kind, amount, allocation.category_id.is_none())?;
                    display_categories
                        .entry(allocation.category_id)
                        .or_default()
                        .add(&c.kind, amount, allocation.category_id.is_none())?;
                }
            }
            if !c.quality_flags.is_empty() {
                quality_affected.push(json!({"transaction_id":c.transaction_id,"component_id":c.component_id,"money":{"currency":c.money.currency,"amount_minor":selected_amount},"quality_flags":c.quality_flags}));
            }
            if r.display_currency.is_some() && conversion.is_none() {
                missing.push(json!({"transaction_id":c.transaction_id,"component_id":c.component_id,"money":c.money,"date":date,"reason":"No actual settlement or quote for occurrence date"}));
            }
            if let Some((_, basis)) = &conversion {
                used_quotes.push(json!({"component_id":c.component_id,"basis":basis}));
            }
            rows.push(json!({"component":c,"selected_amount_minor":selected_amount,"selected_allocations":selected,"display_amount_minor":conversion.as_ref().map(|_|selected_display),"display_allocations":converted_allocations,"conversion_basis":conversion.as_ref().map(|(_,b)|b)}));
        }
        let total_rows = totals
            .into_iter()
            .map(|(currency, t)| {
                let mut v = json!(t);
                v["currency"] = json!(currency);
                v
            })
            .collect::<Vec<_>>();
        let category_rows=categories.into_iter().map(|((category,currency),t)|json!({"category_id":category,"currency":currency,"totals":t})).collect::<Vec<_>>();
        Ok(
            json!({"date_basis":"occurred_on; [from,before); current business versions","totals":total_rows,"by_category":category_rows,"components":rows,"undated_transaction_ids":undated,"quality_affected":quality_affected,"display_totals":r.display_currency.as_ref().map(|currency|json!({"currency":currency,"totals":display,"complete":missing.is_empty(),"missing_components":missing,"bases":used_quotes})),"display_by_category":display_categories.into_iter().map(|(id,t)|json!({"category_id":id,"totals":t})).collect::<Vec<_>>(),"rounding":"half_even component total; largest remainder allocations before filtering"}),
        )
    }
    pub(super) async fn balance_report(&mut self, r: &ReportsGet) -> Result<Value> {
        require(
            r.from.is_none()
                && r.category_id.is_none()
                && r.tag_id.is_none()
                && r.merchant.is_none(),
            "Account balance report accepts before, account and currency filters",
        )?;
        let as_of = r
            .before
            .map(|d| {
                d.pred_opt()
                    .ok_or_else(|| Error::Invalid("Date out of range".into()))
            })
            .transpose()?;
        let balances = self.balances(as_of).await?;
        let openings = self.openings().await?;
        let accounts = self.accounts().await?;
        let timezone: String = sqlx::query_scalar("SELECT timezone FROM ledgers WHERE id=$1")
            .bind(self.ledger_id)
            .fetch_one(&mut *self.tx)
            .await?;
        let zone = timezone
            .parse::<chrono_tz::Tz>()
            .map_err(|_| Error::Invalid("Invalid ledger timezone".into()))?;
        let quote_date =
            as_of.unwrap_or_else(|| chrono::Utc::now().with_timezone(&zone).date_naive());
        let mut rows = vec![];
        let mut estimate = Amount::ZERO;
        let mut missing = vec![];
        for a in accounts.values().filter(|a| {
            r.account_id.is_none_or(|id| id == a.id)
                && r.currency.as_ref().is_none_or(|c| c == &a.currency)
        }) {
            let amount = balances.get(&a.id).copied().unwrap_or_default();
            let mut display = None;
            if let Some(target) = &r.display_currency {
                if a.currency == *target {
                    display = Some(json!({"amount_minor":amount,"basis":"original_currency"}));
                } else if amount == Amount::ZERO {
                    display = Some(json!({"amount_minor":Amount::ZERO,"basis":"zero_balance"}));
                } else if let Some(quote) = self.quote(&a.currency, target, quote_date).await? {
                    display = Some(
                        json!({"amount_minor":convert(amount,&a.currency,target,quote["rate"].as_str().ok_or(Error::NotFound)?)?,"basis":"as_of_quote","quote":quote}),
                    );
                } else {
                    missing.push(a.id);
                }
                if a.role.is_none()
                    && let Some(v) = &display
                {
                    estimate =
                        estimate.checked_add(serde_json::from_value(v["amount_minor"].clone())?)?;
                }
            }
            rows.push(json!({"account":a,"balance_minor":amount,"opening":opening_status(openings.get(&a.id),as_of),"display":display}));
        }
        Ok(
            json!({"date_basis":"posting date; before exclusive; all posted entries including reversals","balance_sign":"debit_positive","accounts":rows,"estimate":r.display_currency.as_ref().map(|c|json!({"currency":c,"amount_minor":estimate,"quote_date":quote_date,"missing_account_ids":missing,"complete":missing.is_empty(),"scope":"real financial accounts; no historical cost or FX gain/loss"}))}),
        )
    }
    pub(super) async fn quality_report(&mut self) -> Result<Value> {
        let mut transactions = vec![];
        for t in self
            .current_transactions()
            .await?
            .into_iter()
            .filter(|t| t.state == "posted")
        {
            let flags = self.transaction_quality(&t).await?;
            if !flags.is_empty() {
                transactions.push(json!({"transaction_id":t.id,"version":t.version,"quality_flags":flags,"money":super::transactions::principal_money(&t.transaction),"review_note":t.transaction.review_note,"next_actions":["Read saved notes and current sources; correct the same transaction with expected_version, or attach current evidence"]}));
            }
        }
        let mut observations = vec![];
        for o in self.observations().await? {
            let status = self.observation_status(&o).await?;
            if status["pending"] == true {
                observations.push(json!({"observation":o,"status":status}));
            }
        }
        let mut checks:Vec<Value>=sqlx::query_scalar("SELECT result||jsonb_build_object('stale',ledger_revision<>$2,'checked_revision',ledger_revision) FROM reconciliations WHERE ledger_id=$1 ORDER BY ledger_revision,id").bind(self.ledger_id).bind(self.revision).fetch_all(&mut *self.tx).await?;
        for check in &mut checks {
            let stale = check["stale"].clone();
            check["data"]["reconciliation"]["stale"] = stale.clone();
            check["data"]["import_completeness"]["stale"] = stale;
        }
        Ok(
            json!({"transactions":transactions,"observations":observations,"checks":checks,"reconciliation":if checks.is_empty(){json!({"status":"not_run"})}else{json!({"status":"scoped_results","results":"checks"})},"import_completeness":if checks.is_empty(){json!({"status":"unknown","reason":"No scoped completeness check has run"})}else{json!({"status":"scoped_results","results":"checks"})},"import_batches":self.batch_summaries().await?}),
        )
    }
    pub(super) async fn context_snapshot(&mut self) -> Result<Value> {
        let ledger = sqlx::query("SELECT reporting_currency,timezone FROM ledgers WHERE id=$1")
            .bind(self.ledger_id)
            .fetch_one(&mut *self.tx)
            .await?;
        let accounts = self.accounts().await?;
        let quality = self.quality_report().await?;
        let unmapped:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('source_id',s.id,'source_version_id',s.current_version_id,'reason','source rows need extraction or stable identity mapping') FROM sources s WHERE s.ledger_id=$1 AND (NOT EXISTS(SELECT 1 FROM source_positions p WHERE p.ledger_id=s.ledger_id AND p.source_version_id=s.current_version_id) OR EXISTS(SELECT 1 FROM observations o JOIN observation_versions v ON v.ledger_id=o.ledger_id AND v.observation_id=o.id AND v.version=o.current_version WHERE o.ledger_id=s.ledger_id AND o.source_id=s.id AND v.source_version_id<>s.current_version_id)) ORDER BY s.id LIMIT 100").bind(self.ledger_id).fetch_all(&mut *self.tx).await?;
        let balances = self.balances(None).await?;
        let openings = self.openings().await?;
        let summaries=accounts.values().filter(|a|a.role.is_none()).take(100).map(|a|json!({"account_id":a.id,"currency":a.currency,"balance_minor":balances.get(&a.id).copied().unwrap_or_default(),"opening":opening_status(openings.get(&a.id),None)})).collect::<Vec<_>>();
        Ok(envelope(
            json!({"ledger_id":self.ledger_id,"reporting_currency":ledger.try_get::<String,_>("reporting_currency")?,"timezone":ledger.try_get::<String,_>("timezone")?,"accounts":accounts.values().take(100).collect::<Vec<_>>(),"accounts_truncated":accounts.len()>100,"account_balances":summaries,"preferences":self.preferences_snapshot().await?,"supported_currencies":CURRENCIES,"currency_table_version":"sumi-currencies-1","capabilities":["accounts","sources","observations","evidence_review","strong_identifiers","expense","income","transfer","refund","fees","categories","tags","fx_reports","reconciliation","import_completeness","export"],"pending_capabilities":[],"pending_transactions":quality["transactions"],"pending_observations":quality["observations"],"checks":quality["checks"],"unmapped_sources":unmapped,"import_batches":quality["import_batches"]}),
            self.revision,
        ))
    }
}
