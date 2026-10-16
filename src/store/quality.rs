use super::*;

fn in_window(date: Option<NaiveDate>, r: &ReportsGet) -> bool {
    r.from.is_none_or(|from| date.is_none_or(|d| d >= from))
        && r.before
            .is_none_or(|before| date.is_none_or(|d| d < before))
}
impl Session {
    pub(super) async fn filtered_quality_report(&mut self, r: &ReportsGet) -> Result<Value> {
        let mut quality = self.quality_report().await?;
        let transactions = self.current_transactions().await?;
        let components = self.components().await?;
        let selected = transactions
            .iter()
            .filter(|t| {
                let d = &t.transaction;
                r.account_id
                    .is_none_or(|id| d.financial_accounts().contains(&id))
                    && r.currency.as_ref().is_none_or(|c| {
                        components
                            .iter()
                            .any(|v| v.transaction_id == t.id && &v.money.currency == c)
                            || super::transactions::principal_money(d)
                                .is_some_and(|m| &m.currency == c)
                    })
                    && r.merchant.as_ref().is_none_or(|m| {
                        d.merchant.as_ref() == Some(m) || d.normalized_merchant.as_ref() == Some(m)
                    })
                    && in_window(d.occurred_on, r)
                    && r.category_id.is_none_or(|id| {
                        components.iter().any(|c| {
                            c.transaction_id == t.id
                                && c.allocations.iter().any(|a| a.category_id == Some(id))
                        })
                    })
                    && r.tag_id.is_none_or(|id| {
                        d.tags.contains(&id)
                            || components.iter().any(|c| {
                                c.transaction_id == t.id
                                    && (c.tags.contains(&id)
                                        || c.allocations.iter().any(|a| a.tags.contains(&id)))
                            })
                    })
            })
            .map(|t| t.id)
            .collect::<std::collections::BTreeSet<_>>();
        quality["transactions"]
            .as_array_mut()
            .ok_or(Error::NotFound)?
            .retain(|t| {
                serde_json::from_value::<Uuid>(t["transaction_id"].clone())
                    .is_ok_and(|id| selected.contains(&id))
            });
        let links = self.evidence_links(None, true).await?;
        let mut selected_observations = std::collections::BTreeSet::new();
        for o in self.observations().await? {
            let account = super::imports::observation_account(&o.payload);
            let (currency, date) = match &o.payload {
                ObservationPayload::Movement {
                    money, posted_on, ..
                } => (money.as_ref().map(|m| m.currency.as_str()), *posted_on),
                ObservationPayload::Balance {
                    currency, as_of, ..
                } => (Some(currency.as_str()), Some(*as_of)),
                ObservationPayload::Statement { statement } => (
                    Some(statement.currency.as_str()),
                    Some(statement.period_start),
                ),
                _ => (None, None),
            };
            if r.account_id.is_some_and(|id| account != Some(id))
                || r.currency
                    .as_ref()
                    .is_some_and(|c| currency != Some(c.as_str()))
                || !in_window(date, r)
            {
                continue;
            }
            if (r.category_id.is_some() || r.tag_id.is_some() || r.merchant.is_some())
                && !links
                    .iter()
                    .any(|l| l.observation_id == o.id && selected.contains(&l.transaction_id))
            {
                continue;
            }
            selected_observations.insert(o.id);
        }
        quality["observations"]
            .as_array_mut()
            .ok_or(Error::NotFound)?
            .retain(|o| {
                serde_json::from_value::<Uuid>(o["observation"]["id"].clone())
                    .is_ok_and(|id| selected_observations.contains(&id))
            });
        quality["checks"]
            .as_array_mut()
            .ok_or(Error::NotFound)?
            .retain(|check| {
                let scope = &check["data"]["scope"];
                r.account_id
                    .is_none_or(|id| scope["account_id"] == json!(id))
                    && r.currency
                        .as_ref()
                        .is_none_or(|c| scope["currency"] == json!(c))
                    && r.from.is_none_or(|date| {
                        serde_json::from_value::<NaiveDate>(scope["period_end"].clone())
                            .is_ok_and(|end| end > date)
                    })
                    && r.before.is_none_or(|date| {
                        serde_json::from_value::<NaiveDate>(scope["period_start"].clone())
                            .is_ok_and(|start| start < date)
                    })
            });
        if quality["checks"].as_array().is_some_and(Vec::is_empty) {
            quality["reconciliation"] = json!({"status":"not_run"});
            quality["import_completeness"] =
                json!({"status":"unknown","reason":"No matching scoped check has run"});
        }
        let scoped = r.account_id.is_some()
            || r.currency.is_some()
            || r.merchant.is_some()
            || r.category_id.is_some()
            || r.tag_id.is_some()
            || r.from.is_some()
            || r.before.is_some();
        if scoped {
            let mut batches = vec![];
            for batch in quality["import_batches"]
                .as_array()
                .ok_or(Error::NotFound)?
            {
                let batch_id: Uuid = serde_json::from_value(batch["batch"]["id"].clone())?;
                let ids:Vec<Uuid>=sqlx::query_scalar("SELECT observation_id FROM batch_observations WHERE ledger_id=$1 AND batch_id=$2").bind(self.ledger_id).bind(batch_id).fetch_all(&mut *self.tx).await?;
                if ids.iter().any(|id| selected_observations.contains(id)) {
                    batches.push(batch.clone());
                }
            }
            quality["import_batches"] = json!(batches);
        }
        let amounts = self.component_report(r).await?;
        quality["affected_amounts"] = amounts["quality_affected"].clone();
        quality["display_totals"] = amounts["display_totals"].clone();
        quality["scope"] = json!({"from":r.from,"before":r.before,"account_id":r.account_id,"currency":r.currency,"merchant":r.merchant,"category_id":r.category_id,"tag_id":r.tag_id,"checks":"Each check retains its own account, statement and period scope"});
        Ok(quality)
    }
}
