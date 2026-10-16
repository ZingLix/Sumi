use super::*;

fn rule(code: &'static str, message: impl Into<String>, entities: Value) -> Error {
    Error::Rule {
        code,
        message: message.into(),
        entities,
    }
}
impl Store {
    pub(super) async fn apply_complete(
        &self,
        p: &Principal,
        request: TransactionsApply,
    ) -> Result<Value> {
        require(
            !request.actions.is_empty() && request.actions.len() <= 50,
            "Submit 1..50 transaction actions",
        )?;
        let raw = serde_json::to_value(&request)?;
        let fingerprint = hash(&raw)?;
        let tool = "sumi_transactions_apply";
        let operation_id = Uuid::new_v4();
        let mut s = self.session(p, request.ledger_id, true).await?;
        if !request.preview {
            if let Some(v) = s
                .replay(p, tool, &request.idempotency_key, &fingerprint)
                .await?
            {
                return Ok(v);
            }
        } else {
            validate_key(&request.idempotency_key)?;
        }
        let accounts = s.accounts().await?;
        let mut changed = false;
        let mut results = vec![];
        let mut touched = std::collections::BTreeSet::new();
        for action in request.actions {
            let (mut id, expected, mut data, reason, state, mut is_new, mut deduplicated) =
                match action {
                    Action::Create { transaction } => {
                        require(
                            transaction.opening_account().is_none(),
                            "Use set_opening for a baseline",
                        )?;
                        (
                            Uuid::new_v4(),
                            None,
                            Some(transaction),
                            "Create".into(),
                            "posted",
                            true,
                            false,
                        )
                    }
                    Action::Revise {
                        transaction_id,
                        expected_version,
                        reason,
                        transaction,
                    } => {
                        require(
                            transaction.opening_account().is_none(),
                            "Use set_opening for a baseline",
                        )?;
                        (
                            transaction_id,
                            Some(expected_version),
                            Some(transaction),
                            reason,
                            "posted",
                            false,
                            false,
                        )
                    }
                    Action::Void {
                        transaction_id,
                        expected_version,
                        reason,
                    } => (
                        transaction_id,
                        Some(expected_version),
                        None,
                        reason,
                        "voided",
                        false,
                        false,
                    ),
                    Action::SetOpening {
                        account_id,
                        money,
                        as_of,
                        transaction_id,
                        expected_version,
                        reason,
                    } => {
                        require(
                            transaction_id.is_some() == expected_version.is_some(),
                            "Opening correction needs both transaction_id and expected_version",
                        )?;
                        let data: TransactionData = serde_json::from_value(
                            json!({"occurred_on":as_of,"description":"Opening balance","movement":{"kind":"opening","account_id":account_id,"money":money,"as_of":as_of}}),
                        )?;
                        (
                            transaction_id.unwrap_or_else(Uuid::new_v4),
                            expected_version,
                            Some(data),
                            reason,
                            "posted",
                            transaction_id.is_none(),
                            false,
                        )
                    }
                    Action::LinkEvidence {
                        transaction_id,
                        expected_version,
                        reason,
                        evidence_refs,
                    } => {
                        let mut current = s.get_current(transaction_id).await?.transaction;
                        let refs = current.evidence_refs.get_or_insert_default();
                        let incoming = evidence_refs
                            .iter()
                            .map(|r| r.observation_id)
                            .collect::<std::collections::BTreeSet<_>>();
                        refs.retain(|r| !incoming.contains(&r.observation_id));
                        refs.extend(evidence_refs);
                        (
                            transaction_id,
                            Some(expected_version),
                            Some(current),
                            reason,
                            "posted",
                            false,
                            false,
                        )
                    }
                    Action::UnlinkEvidence {
                        transaction_id,
                        expected_version,
                        reason,
                        observation_ids,
                    } => {
                        let mut current = s.get_current(transaction_id).await?.transaction;
                        current
                            .evidence_refs
                            .get_or_insert_default()
                            .retain(|r| !observation_ids.contains(&r.observation_id));
                        (
                            transaction_id,
                            Some(expected_version),
                            Some(current),
                            reason,
                            "posted",
                            false,
                            false,
                        )
                    }
                };
            require(
                !reason.trim().is_empty() && reason.len() <= 4096,
                "Transaction writes need a reason",
            )?;
            if is_new
                && let Some(incoming) = &data
                && let Some(existing) = s.find_identifier_transaction(&incoming.identifiers).await?
            {
                id = existing;
                is_new = false;
                deduplicated = true;
            }
            require(
                touched.insert(id),
                "Modify each transaction only once per apply",
            )?;
            let old = if is_new {
                None
            } else {
                Some(s.get_current(id).await?)
            };
            if !deduplicated && let Some(old) = &old {
                version(
                    expected
                        .ok_or_else(|| Error::Invalid("expected_version is required".into()))?,
                    old.version,
                )?;
            }
            if let Some(old) = &old
                && let Some(data) = &data
            {
                require(
                    old.transaction.opening_account() == data.opening_account(),
                    "Opening identity cannot change",
                )?;
                require(
                    old.state == "posted" || old.transaction.opening_account().is_some(),
                    "Voided transaction identities cannot be reused",
                )?;
            }
            let mut data = data
                .take()
                .or_else(|| old.as_ref().map(|o| o.transaction.clone()))
                .ok_or(Error::NotFound)?;
            if let Some(old) = &old {
                preserve_omitted_allocations(&mut data, &old.transaction)?;
            }
            s.normalize_transaction(id, &mut data, old.as_ref().map(|o| &o.transaction))
                .await?;
            if let Movement::Refund {
                original_transaction_id,
                original_money,
                refund_allocations,
                ..
            } = &mut data.movement
                && refund_allocations.is_empty()
            {
                let original = s.get_current(*original_transaction_id).await?;
                if original.transaction.allocations.len() == 1 {
                    refund_allocations.push(RefundAllocation {
                        allocation_id: original.transaction.allocations[0]
                            .allocation_id
                            .ok_or(Error::NotFound)?,
                        amount_minor: original_money.amount_minor,
                    });
                }
            }
            let mut planning_accounts = accounts.clone();
            if let Some(old) = &old {
                for id in old.transaction.financial_accounts() {
                    if let Some(a) = planning_accounts.get_mut(&id) {
                        a.active = true;
                    }
                }
            }
            let new_plan = if state == "posted" {
                data.plan(s.ledger_id, &planning_accounts)?
            } else {
                vec![]
            };
            let old_entries = s.active_entries(id).await?;
            let old_plan = old_entries
                .iter()
                .map(|(_, e)| e.clone())
                .collect::<Vec<_>>();
            if deduplicated {
                if old_plan != new_plan {
                    return Err(rule(
                        "STRONG_IDENTIFIER_CONFLICT",
                        "A strong identifier already refers to different financial facts",
                        json!({"transaction_id":id,"current_version":old.as_ref().map(|o|o.version)}),
                    ));
                }
                let incoming = data;
                data = old.as_ref().ok_or(Error::NotFound)?.transaction.clone();
                data.identifiers.extend(incoming.identifiers);
                data.identifiers.sort_by_key(|i| {
                    (
                        i.namespace.clone(),
                        i.account_id,
                        i.event_type.clone(),
                        i.external_id.clone(),
                    )
                });
                data.identifiers.dedup();
                if let Some(new_refs) = incoming.evidence_refs {
                    let refs = data.evidence_refs.get_or_insert_default();
                    for reference in new_refs {
                        if !refs.contains(&reference) {
                            refs.push(reference);
                        }
                    }
                }
            }
            let financial_change = old_plan != new_plan;
            let old_refs = old
                .as_ref()
                .and_then(|o| o.transaction.evidence_refs.as_deref())
                .unwrap_or(&[]);
            if data.evidence_refs.is_none() {
                data.evidence_refs = Some(if financial_change {
                    vec![]
                } else {
                    old_refs.to_vec()
                });
            }
            if state == "voided" {
                data.evidence_refs = Some(vec![]);
            }
            s.canonical_evidence(
                data.evidence_refs.as_mut().ok_or(Error::NotFound)?,
                old_refs,
                financial_change,
            )
            .await?;
            let data_changed = old
                .as_ref()
                .is_none_or(|o| o.transaction != data || o.state != state);
            if !data_changed {
                let old = old.ok_or(Error::NotFound)?;
                results.push(json!({"transaction_id":id,"version":old.version,"state":old.state,"changed":false,"deduplicated":deduplicated}));
                continue;
            }
            let next_version = old
                .as_ref()
                .map_or(Ok(1), |o| o.version.checked_add(1).ok_or(Error::Overflow))?;
            let revision = s.revision.checked_add(1).ok_or(Error::Overflow)?;
            if is_new {
                if let Some(account) = data.opening_account() {
                    let exists:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM transactions WHERE ledger_id=$1 AND opening_account_id=$2)").bind(s.ledger_id).bind(account).fetch_one(&mut *s.tx).await?;
                    if exists {
                        return Err(Error::Conflict(
                            "Account has an opening baseline; revise it with its current version"
                                .into(),
                        ));
                    }
                }
                sqlx::query("INSERT INTO transactions(ledger_id,id,current_version,state,opening_account_id) VALUES($1,$2,$3,$4,$5)").bind(s.ledger_id).bind(id).bind(next_version).bind(state).bind(data.opening_account()).execute(&mut *s.tx).await?;
            } else {
                sqlx::query("UPDATE transactions SET current_version=$3,state=$4 WHERE ledger_id=$1 AND id=$2").bind(s.ledger_id).bind(id).bind(next_version).bind(state).execute(&mut *s.tx).await?;
            }
            sqlx::query("INSERT INTO transaction_versions(ledger_id,transaction_id,version,data,state,reason,operation_id,recorded_revision) VALUES($1,$2,$3,$4,$5,$6,$7,$8)").bind(s.ledger_id).bind(id).bind(next_version).bind(serde_json::to_value(&data)?).bind(state).bind(reason).bind(operation_id).bind(revision).execute(&mut *s.tx).await?;
            let mut appended = vec![];
            if financial_change {
                for (target, entry) in old_entries {
                    let reversal = entry.reversed()?;
                    let eid = s
                        .append_entry(
                            id,
                            next_version,
                            revision,
                            operation_id,
                            &reversal,
                            Some(target),
                        )
                        .await?;
                    appended.push(json!({"id":eid,"reverses_entry_id":target,"entry":reversal}));
                }
                for entry in new_plan {
                    s.ensure_system_accounts(&entry).await?;
                    let eid = s
                        .append_entry(id, next_version, revision, operation_id, &entry, None)
                        .await?;
                    appended.push(json!({"id":eid,"entry":entry}));
                }
            }
            s.sync_evidence(
                id,
                data.evidence_refs.as_deref().unwrap_or(&[]),
                operation_id,
                financial_change,
            )
            .await?;
            s.save_identifiers(id, &data.identifiers).await?;
            changed = true;
            results.push(json!({"transaction_id":id,"version":next_version,"state":state,"transaction":data,"changed":true,"deduplicated":deduplicated,"appended_entries":appended}));
        }
        s.validate_tracking_baselines().await?;
        s.validate_refunds().await?;
        s.validate_evidence(operation_id).await?;
        if changed {
            s.bump().await?;
        }
        for result in &mut results {
            let id: Uuid = serde_json::from_value(result["transaction_id"].clone())?;
            let current = s.get_current(id).await?;
            result["quality_flags"] = json!(s.transaction_quality(&current).await?);
            result["duplicate_candidates"] = json!(s.duplicate_candidates(&current).await?);
        }
        let mut response = envelope(
            json!({"transactions":results,"preview":request.preview}),
            s.revision,
        );
        if request.preview {
            response["changed"] = json!(false);
            response["ledger_revision"] = json!(s.revision - i64::from(changed));
            response["data"]["would_change"] = json!(changed);
            response["data"]["proposed_revision"] = json!(s.revision);
            sqlx::query("INSERT INTO operations(ledger_id,id,user_id,client_id,tool,idempotency_key,request_hash,request,result) VALUES($1,$2,$3,$4,'preview',$5,$6,$7,$8)").bind(s.ledger_id).bind(operation_id).bind(p.user_id()).bind(&p.client_id).bind(operation_id.to_string()).bind(&fingerprint).bind(raw).bind(&response).execute(&mut *s.tx).await?;
            sqlx::query("SET CONSTRAINTS ALL IMMEDIATE")
                .execute(&mut *s.tx)
                .await?;
            s.tx.rollback().await?;
            Ok(response)
        } else {
            s.finish(
                WriteOperation {
                    principal: p,
                    tool,
                    key: &request.idempotency_key,
                    request_hash: &fingerprint,
                    request: raw,
                },
                response,
                changed,
                operation_id,
            )
            .await
        }
    }
}
fn preserve_omitted_allocations(data: &mut TransactionData, old: &TransactionData) -> Result<()> {
    if data.allocations.is_empty()
        && let Some(money) = principal_money(data)
        && principal_money(old).is_some_and(|m| m.currency == money.currency)
        && !old.allocations.is_empty()
    {
        data.allocations = old.allocations.clone();
        if data.allocations.len() == 1 {
            data.allocations[0].amount_minor = money.amount_minor;
        }
    }
    for (i, fee) in data.fees.iter_mut().enumerate() {
        if fee.allocations.is_empty()
            && let Some(previous) = old
                .fees
                .iter()
                .find(|f| f.fee_id == fee.fee_id && fee.fee_id.is_some())
                .or_else(|| old.fees.get(i))
            && fee.money.currency == previous.money.currency
        {
            fee.allocations = previous.allocations.clone();
            if fee.allocations.len() == 1 {
                fee.allocations[0].amount_minor = fee.money.amount_minor;
            }
        }
    }
    Ok(())
}
pub(super) fn principal_money(data: &TransactionData) -> Option<Money> {
    match &data.movement {
        Movement::Expense { money, .. }
        | Movement::Income { money, .. }
        | Movement::Refund { money, .. }
        | Movement::Opening { money, .. }
        | Movement::UnresolvedMovement { money, .. } => Some(money.clone()),
        Movement::Transfer { from_money, .. } => Some(from_money.clone()),
    }
}
impl Session {
    pub(super) async fn find_identifier_transaction(
        &mut self,
        identifiers: &[StrongIdentifier],
    ) -> Result<Option<Uuid>> {
        let accounts = self.accounts().await?;
        let mut found = std::collections::BTreeSet::new();
        for i in identifiers {
            require(
                [&i.namespace, &i.event_type, &i.external_id]
                    .iter()
                    .all(|v| !v.trim().is_empty() && v.len() <= 512),
                "Strong identifier fields require 1..512 bytes",
            )?;
            require(
                accounts
                    .get(&i.account_id)
                    .is_some_and(|a| a.role.is_none()),
                "Strong identifier requires a financial account in this ledger",
            )?;
            let id:Option<Uuid>=sqlx::query_scalar("SELECT transaction_id FROM transaction_identifiers WHERE ledger_id=$1 AND namespace=$2 AND account_id=$3 AND event_type=$4 AND external_id=$5").bind(self.ledger_id).bind(&i.namespace).bind(i.account_id).bind(&i.event_type).bind(&i.external_id).fetch_optional(&mut *self.tx).await?;
            if let Some(id) = id {
                found.insert(id);
            }
        }
        if found.len() > 1 {
            return Err(rule(
                "STRONG_IDENTIFIER_CONFLICT",
                "Identifiers refer to multiple existing transactions",
                json!({"transaction_ids":found}),
            ));
        }
        Ok(found.into_iter().next())
    }
    pub(super) async fn save_identifiers(
        &mut self,
        id: Uuid,
        identifiers: &[StrongIdentifier],
    ) -> Result<()> {
        if let Some(existing) = self.find_identifier_transaction(identifiers).await?
            && existing != id
        {
            return Err(rule(
                "STRONG_IDENTIFIER_CONFLICT",
                "Strong identifier is permanently assigned to another transaction",
                json!({"transaction_id":existing}),
            ));
        }
        let current = self.get_current(id).await?;
        for i in identifiers {
            let existing:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM transaction_identifiers WHERE ledger_id=$1 AND namespace=$2 AND account_id=$3 AND event_type=$4 AND external_id=$5)").bind(self.ledger_id).bind(&i.namespace).bind(i.account_id).bind(&i.event_type).bind(&i.external_id).fetch_one(&mut *self.tx).await?;
            if !existing {
                require(
                    current
                        .transaction
                        .financial_accounts()
                        .contains(&i.account_id),
                    "Strong identifier account must participate in the transaction",
                )?;
            }
            sqlx::query("INSERT INTO transaction_identifiers(ledger_id,namespace,account_id,event_type,external_id,transaction_id) VALUES($1,$2,$3,$4,$5,$6) ON CONFLICT DO NOTHING").bind(self.ledger_id).bind(&i.namespace).bind(i.account_id).bind(&i.event_type).bind(&i.external_id).bind(id).execute(&mut *self.tx).await?;
        }
        Ok(())
    }
    pub(super) async fn validate_tracking_baselines(&mut self) -> Result<()> {
        let openings = self.openings().await?;
        for t in self.current_transactions().await?.into_iter().filter(|t| {
            t.state == "posted" && !matches!(t.transaction.movement, Movement::Opening { .. })
        }) {
            for leg in self.funding_legs(t.id).await? {
                if let Some((_, as_of)) = openings.get(&leg.account_id) {
                    require(
                        leg.posted_on > *as_of,
                        "A financial movement cannot precede its confirmed tracking baseline; correct the baseline or posting date",
                    )?;
                }
            }
        }
        Ok(())
    }
    pub(super) async fn validate_refunds(&mut self) -> Result<()> {
        let transactions = self
            .current_transactions()
            .await?
            .into_iter()
            .map(|t| (t.id, t))
            .collect::<BTreeMap<_, _>>();
        let mut amounts = BTreeMap::<(Uuid, Uuid), Amount>::new();
        for t in transactions.values().filter(|t| t.state == "posted") {
            if let Movement::Refund {
                original_transaction_id,
                original_money,
                refund_allocations,
                ..
            } = &t.transaction.movement
            {
                let original = transactions
                    .get(original_transaction_id)
                    .ok_or(Error::NotFound)?;
                require(
                    original.state == "posted",
                    "Cannot refund a voided original expense",
                )?;
                let money = if let Movement::Expense { money, .. } = &original.transaction.movement
                {
                    money
                } else {
                    return Err(Error::Invalid("Refund must refer to an expense".into()));
                };
                require(
                    original_money.currency == money.currency,
                    "Refunded principal must use original expense currency",
                )?;
                let mut seen = std::collections::BTreeSet::new();
                let mut total = Amount::ZERO;
                for refund in refund_allocations {
                    require(
                        seen.insert(refund.allocation_id),
                        "Duplicate refund allocation",
                    )?;
                    refund.amount_minor.positive()?;
                    total = total.checked_add(refund.amount_minor)?;
                    let source=original.transaction.allocations.iter().find(|a|a.allocation_id==Some(refund.allocation_id)).ok_or_else(||rule("REFUND_ALLOCATION_CONFLICT","Refund allocation no longer exists on the original expense",json!({"original_transaction_id":original.id,"allocation_id":refund.allocation_id})))?;
                    let used = amounts
                        .entry((original.id, refund.allocation_id))
                        .or_default();
                    *used = used.checked_add(refund.amount_minor)?;
                    if *used > source.amount_minor {
                        return Err(rule(
                            "REFUND_OVERALLOCATED",
                            "Cumulative refunds exceed the original allocation",
                            json!({"original_transaction_id":original.id,"allocation_id":refund.allocation_id,"refunded_minor":used,"available_minor":source.amount_minor}),
                        ));
                    }
                }
                require(
                    total == original_money.amount_minor,
                    "Refund allocation total differs from principal",
                )?;
            }
        }
        Ok(())
    }
    pub(super) async fn duplicate_candidates(
        &mut self,
        t: &CurrentTransaction,
    ) -> Result<Vec<Value>> {
        let money = principal_money(&t.transaction);
        if money.is_none() || t.transaction.merchant.is_none() {
            return Ok(vec![]);
        }
        let rows=sqlx::query("SELECT t.id,t.current_version,t.state,v.data FROM transactions t JOIN transaction_versions v ON v.ledger_id=t.ledger_id AND v.transaction_id=t.id AND v.version=t.current_version WHERE t.ledger_id=$1 AND t.id<>$2 AND t.state='posted' AND v.data->>'merchant'=$3 AND v.data->'occurred_on'=$4 AND coalesce(v.data->'movement'->'money',v.data->'movement'->'from_money')=$5 ORDER BY t.id LIMIT 100")
            .bind(self.ledger_id).bind(t.id).bind(&t.transaction.merchant).bind(json!(t.transaction.occurred_on)).bind(json!(money)).fetch_all(&mut *self.tx).await?;
        let candidates = rows
            .into_iter()
            .map(current_row)
            .collect::<Result<Vec<_>>>()?;
        Ok(candidates.into_iter().map(|o|json!({"transaction_id":o.id,"version":o.version,"review_note":o.transaction.review_note,"transaction":o.transaction})).collect())
    }
    pub(super) async fn refund_relationships(&mut self, id: Uuid) -> Result<Value> {
        let current = self.get_current(id).await?;
        if !matches!(
            current.transaction.movement,
            Movement::Expense { .. } | Movement::Refund { .. }
        ) {
            return Ok(json!({"eligible":false,"refunds":[],"allocations":[]}));
        }
        let original_id = match current.transaction.movement {
            Movement::Refund {
                original_transaction_id,
                ..
            } => original_transaction_id,
            _ => id,
        };
        let original = self.get_current(original_id).await?;
        let mut refunded = BTreeMap::<Uuid, Amount>::new();
        let mut relations = vec![];
        for t in self.current_transactions().await? {
            if let Movement::Refund {
                original_transaction_id,
                refund_allocations,
                money,
                original_money,
                ..
            } = &t.transaction.movement
                && *original_transaction_id == original_id
            {
                if t.state == "posted" {
                    for a in refund_allocations {
                        let total = refunded.entry(a.allocation_id).or_default();
                        *total = total.checked_add(a.amount_minor)?;
                    }
                }
                relations.push(json!({"transaction_id":t.id,"version":t.version,"state":t.state,"actual_receipt":money,"original_principal":original_money,"refund_allocations":refund_allocations}));
            }
        }
        let allocations=original.transaction.allocations.iter().map(|a|{let allocation_id=a.allocation_id.ok_or(Error::NotFound)?;let amount=refunded.get(&allocation_id).copied().unwrap_or_default();Ok(json!({"allocation_id":allocation_id,"category_id":a.category_id,"original_minor":a.amount_minor,"refunded_minor":amount,"remaining_minor":a.amount_minor.checked_add(amount.checked_neg()?)?}))}).collect::<Result<Vec<_>>>()?;
        Ok(
            json!({"original_transaction_id":original_id,"refunds":relations,"allocations":allocations}),
        )
    }
}
