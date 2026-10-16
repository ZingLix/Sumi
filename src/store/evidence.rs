use super::imports::Observed;
use super::*;

#[derive(Debug, Clone, Serialize)]
pub(super) struct FundingLeg {
    pub transaction_id: Uuid,
    pub account_id: Uuid,
    pub currency: String,
    pub posted_on: NaiveDate,
    pub amount_minor: Amount,
}
#[derive(Debug, Clone, Serialize)]
pub(super) struct EvidenceLink {
    pub id: Uuid,
    pub transaction_id: Uuid,
    pub observation_id: Uuid,
    pub observation_version: i64,
    pub account_id: Option<Uuid>,
    pub role: String,
    pub currency: Option<String>,
    pub direction: Option<String>,
    pub amount_minor: Option<Amount>,
    pub opened_operation_id: Uuid,
    pub closed_revision: Option<i64>,
}
impl EvidenceLink {
    fn reference(&self) -> EvidenceRef {
        EvidenceRef {
            observation_id: self.observation_id,
            version: self.observation_version,
            account_id: self.account_id,
            amount_minor: self.amount_minor,
            role: if self.role == "movement" {
                EvidenceRole::Movement
            } else {
                EvidenceRole::Context
            },
        }
    }
}
fn rule(code: &'static str, message: impl Into<String>, entities: Value) -> Error {
    Error::Rule {
        code,
        message: message.into(),
        entities,
    }
}
fn absolute(amount: Amount) -> Result<Amount> {
    if amount < Amount::ZERO {
        amount.checked_neg()
    } else {
        Ok(amount)
    }
}
impl Session {
    pub(super) async fn evidence_links(
        &mut self,
        transaction_id: Option<Uuid>,
        active_only: bool,
    ) -> Result<Vec<EvidenceLink>> {
        self.evidence_filtered(transaction_id, None, active_only)
            .await
    }
    async fn evidence_filtered(
        &mut self,
        transaction_id: Option<Uuid>,
        observation_id: Option<Uuid>,
        active_only: bool,
    ) -> Result<Vec<EvidenceLink>> {
        let rows=sqlx::query("SELECT id,transaction_id,observation_id,observation_version,account_id,role,currency,direction,amount_minor::text AS amount,opened_operation_id,closed_revision FROM evidence_links WHERE ledger_id=$1 AND ($2::uuid IS NULL OR transaction_id=$2) AND (NOT $3 OR closed_revision IS NULL) AND ($4::uuid IS NULL OR observation_id=$4) ORDER BY id")
            .bind(self.ledger_id).bind(transaction_id).bind(active_only).bind(observation_id).fetch_all(&mut *self.tx).await?;
        rows.into_iter()
            .map(|r| {
                Ok(EvidenceLink {
                    id: r.try_get("id")?,
                    transaction_id: r.try_get("transaction_id")?,
                    observation_id: r.try_get("observation_id")?,
                    observation_version: r.try_get("observation_version")?,
                    account_id: r.try_get("account_id")?,
                    role: r.try_get("role")?,
                    currency: r.try_get("currency")?,
                    direction: r.try_get("direction")?,
                    amount_minor: r
                        .try_get::<Option<String>, _>("amount")?
                        .map(|s| s.parse())
                        .transpose()?,
                    opened_operation_id: r.try_get("opened_operation_id")?,
                    closed_revision: r.try_get("closed_revision")?,
                })
            })
            .collect()
    }
    pub(super) async fn funding_legs(&mut self, id: Uuid) -> Result<Vec<FundingLeg>> {
        let accounts = self.accounts().await?;
        let mut sums = BTreeMap::<(Uuid, String, NaiveDate), Amount>::new();
        for (_, entry) in self.active_entries(id).await? {
            for line in entry.lines {
                if accounts
                    .get(&line.account_id)
                    .is_some_and(|a| a.role.is_none())
                {
                    let total = sums
                        .entry((line.account_id, line.currency, entry.effective_on))
                        .or_default();
                    *total = total.checked_add(line.amount_minor)?;
                }
            }
        }
        Ok(sums
            .into_iter()
            .filter(|(_, v)| *v != Amount::ZERO)
            .map(
                |((account_id, currency, posted_on), amount_minor)| FundingLeg {
                    transaction_id: id,
                    account_id,
                    currency,
                    posted_on,
                    amount_minor,
                },
            )
            .collect())
    }
    pub(super) async fn source_current(&mut self, o: &Observed) -> Result<bool> {
        let current: Uuid = sqlx::query_scalar(
            "SELECT current_version_id FROM sources WHERE ledger_id=$1 AND id=$2",
        )
        .bind(self.ledger_id)
        .bind(o.source_id)
        .fetch_one(&mut *self.tx)
        .await?;
        Ok(current == o.source_version_id)
    }
    pub(super) async fn canonical_evidence(
        &mut self,
        refs: &mut Vec<EvidenceRef>,
        old_refs: &[EvidenceRef],
        financial_change: bool,
    ) -> Result<()> {
        require(
            refs.len() <= 100,
            "At most 100 evidence references per transaction",
        )?;
        for reference in refs.iter_mut() {
            if !financial_change && old_refs.contains(reference) {
                continue;
            }
            let o = self.observation(reference.observation_id).await?;
            if reference.version != o.current_version || !self.source_current(&o).await? {
                return Err(rule(
                    "EVIDENCE_REVIEW_REQUIRED",
                    "New evidence must reference the current extraction and source version",
                    json!({"observation_id":o.id,"current_version":o.current_version,"source_version_id":o.source_version_id}),
                ));
            }
            if reference.role == EvidenceRole::Movement {
                if let ObservationPayload::Movement {
                    account_id: Some(account),
                    money: Some(m),
                    direction: Some(_),
                    posted_on: Some(_),
                    ..
                } = &o.payload
                {
                    reference.account_id = Some(reference.account_id.unwrap_or(*account));
                    reference.amount_minor = Some(reference.amount_minor.unwrap_or(m.amount_minor));
                    require(
                        reference.account_id == Some(*account),
                        "Evidence financial account differs from observation",
                    )?;
                    reference.amount_minor.ok_or(Error::NotFound)?.positive()?;
                } else {
                    return Err(rule(
                        "EVIDENCE_INVALID",
                        "Funding evidence requires a complete movement with account, amount, direction and posting date",
                        json!({"observation_id":o.id}),
                    ));
                }
            } else {
                require(
                    reference.amount_minor.is_none() && reference.account_id.is_none(),
                    "Context evidence cannot allocate funds",
                )?;
            }
        }
        refs.sort_by_key(|r| {
            (
                r.observation_id,
                r.version,
                r.account_id,
                r.role == EvidenceRole::Movement,
            )
        });
        let mut canonical = BTreeMap::new();
        for reference in std::mem::take(refs) {
            let key = (
                reference.observation_id,
                reference.version,
                reference.account_id,
                reference.role == EvidenceRole::Movement,
            );
            if let Some(existing) = canonical.get(&key) {
                require(
                    existing == &reference,
                    "Conflicting duplicate evidence allocation",
                )?;
            }
            canonical.insert(key, reference);
        }
        *refs = canonical.into_values().collect();
        Ok(())
    }
    pub(super) async fn sync_evidence(
        &mut self,
        id: Uuid,
        refs: &[EvidenceRef],
        operation_id: Uuid,
        financial_change: bool,
    ) -> Result<()> {
        let old = self.evidence_links(Some(id), true).await?;
        let revision = self.revision.checked_add(1).ok_or(Error::Overflow)?;
        for link in &old {
            if financial_change || !refs.contains(&link.reference()) {
                sqlx::query("UPDATE evidence_links SET closed_revision=$3,closed_operation_id=$4 WHERE ledger_id=$1 AND id=$2").bind(self.ledger_id).bind(link.id).bind(revision).bind(operation_id).execute(&mut *self.tx).await?;
            }
        }
        for reference in refs {
            if !financial_change && old.iter().any(|l| l.reference() == *reference) {
                continue;
            }
            let observation = self.observation(reference.observation_id).await?;
            let (currency, direction) = match (&reference.role, &observation.payload) {
                (
                    EvidenceRole::Movement,
                    ObservationPayload::Movement {
                        money: Some(m),
                        direction: Some(d),
                        ..
                    },
                ) => (
                    Some(m.currency.clone()),
                    Some(if *d == Direction::In { "in" } else { "out" }),
                ),
                _ => (None, None),
            };
            let amount = reference
                .amount_minor
                .map(|a| a.to_string().parse::<bigdecimal::BigDecimal>())
                .transpose()
                .map_err(|_| Error::Overflow)?;
            sqlx::query("INSERT INTO evidence_links(ledger_id,id,transaction_id,observation_id,observation_version,account_id,role,currency,direction,amount_minor,opened_revision,opened_operation_id) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12)")
                .bind(self.ledger_id).bind(Uuid::new_v4()).bind(id).bind(reference.observation_id).bind(reference.version).bind(reference.account_id).bind(if reference.role==EvidenceRole::Movement{"movement"}else{"context"}).bind(currency).bind(direction).bind(amount).bind(revision).bind(operation_id).execute(&mut *self.tx).await?;
        }
        Ok(())
    }
    pub(super) async fn validate_evidence(&mut self, operation_id: Uuid) -> Result<()> {
        let links = self.evidence_links(None, true).await?;
        let observations = self
            .observations()
            .await?
            .into_iter()
            .map(|o| (o.id, o))
            .collect::<BTreeMap<_, _>>();
        for o in observations.values() {
            let allocations = links
                .iter()
                .filter(|l| l.observation_id == o.id && l.role == "movement")
                .collect::<Vec<_>>();
            let has_new = allocations
                .iter()
                .any(|l| l.opened_operation_id == operation_id);
            let stale = allocations
                .iter()
                .any(|l| l.observation_version != o.current_version)
                || !self.source_current(o).await?;
            if has_new && stale {
                return Err(rule(
                    "EVIDENCE_REVIEW_REQUIRED",
                    "Resolve all stale allocations of this logical observation in the same apply before creating new allocations",
                    json!({"observation_id":o.id,"transactions":allocations.iter().map(|l|l.transaction_id).collect::<Vec<_>>()}),
                ));
            }
            if has_new {
                let total = allocations.iter().try_fold(Amount::ZERO, |a, l| {
                    a.checked_add(l.amount_minor.ok_or(Error::NotFound)?)
                })?;
                let available = match &o.payload {
                    ObservationPayload::Movement { money: Some(m), .. } => m.amount_minor,
                    _ => return Err(Error::Invalid("Evidence is no longer a movement".into())),
                };
                if total > available {
                    return Err(rule(
                        "EVIDENCE_OVERALLOCATED",
                        "Observation allocations exceed its current amount",
                        json!({"observation_id":o.id,"occupied_minor":total,"available_minor":available}),
                    ));
                }
            }
        }
        // Independent sources are checked independently; duplicated evidence never doubles a transaction.
        let mut by_leg = BTreeMap::<(Uuid, Uuid, String, NaiveDate, Uuid), Amount>::new();
        for link in links.iter().filter(|l| l.role == "movement") {
            let o = observations
                .get(&link.observation_id)
                .ok_or(Error::NotFound)?;
            if link.observation_version != o.current_version || !self.source_current(o).await? {
                continue;
            }
            let (account, money, direction, posted_on) = match &o.payload {
                ObservationPayload::Movement {
                    account_id: Some(a),
                    money: Some(m),
                    direction: Some(d),
                    posted_on: Some(date),
                    ..
                } => (*a, m, *d, *date),
                _ => continue,
            };
            if link.opened_operation_id == operation_id {
                let legs = self.funding_legs(link.transaction_id).await?;
                let target = legs.iter().find(|l| {
                    l.account_id == account
                        && l.currency == money.currency
                        && l.posted_on == posted_on
                        && ((l.amount_minor > Amount::ZERO) == (direction == Direction::In))
                });
                if target.is_none() {
                    return Err(rule(
                        "EVIDENCE_MISMATCH",
                        "Evidence must match a current financial leg's account, currency, direction and posting date",
                        json!({"observation_id":o.id,"transaction_id":link.transaction_id}),
                    ));
                }
            }
            let total = by_leg
                .entry((
                    link.transaction_id,
                    account,
                    money.currency.clone(),
                    posted_on,
                    o.source_id,
                ))
                .or_default();
            *total = total.checked_add(link.amount_minor.ok_or(Error::NotFound)?)?;
        }
        for ((id, account, currency, date, _source), total) in by_leg {
            let leg = self
                .funding_legs(id)
                .await?
                .into_iter()
                .find(|l| l.account_id == account && l.currency == currency && l.posted_on == date)
                .ok_or(Error::NotFound)?;
            if total > absolute(leg.amount_minor)? {
                return Err(rule(
                    "EVIDENCE_OVERALLOCATED",
                    "One source covers more than the financial leg",
                    json!({"transaction_id":id,"account_id":account,"amount_minor":total}),
                ));
            }
        }
        Ok(())
    }
    pub(super) async fn observation_status(&mut self, o: &Observed) -> Result<Value> {
        let links = self
            .evidence_filtered(None, Some(o.id), true)
            .await?
            .into_iter()
            .collect::<Vec<_>>();
        let stale = links
            .iter()
            .any(|l| l.observation_version != o.current_version)
            || !self.source_current(o).await?;
        let occupied = links
            .iter()
            .filter(|l| l.role == "movement")
            .try_fold(Amount::ZERO, |a, l| {
                a.checked_add(l.amount_minor.ok_or(Error::NotFound)?)
            })?;
        let available = match &o.payload {
            ObservationPayload::Movement { money: Some(m), .. } => Some(m.amount_minor),
            _ => None,
        };
        let fact_conflict =
            links
                .iter()
                .filter(|l| l.role == "movement")
                .any(|l| match &o.payload {
                    ObservationPayload::Movement {
                        account_id,
                        money,
                        direction,
                        ..
                    } => {
                        l.account_id != *account_id
                            || money
                                .as_ref()
                                .is_none_or(|m| l.currency.as_ref() != Some(&m.currency))
                            || direction.is_none_or(|d| {
                                l.direction.as_deref()
                                    != Some(if d == Direction::In { "in" } else { "out" })
                            })
                    }
                    _ => true,
                });
        let overallocated = available.is_some_and(|a| occupied > a);
        let pending = o.ignored_reason.is_none()
            && (stale
                || overallocated
                || fact_conflict
                || (matches!(o.payload, ObservationPayload::Movement { .. })
                    && available.is_none_or(|a| occupied < a)
                    || matches!(
                        o.payload,
                        ObservationPayload::Order { .. } | ObservationPayload::Authorization { .. }
                    ) && links.is_empty()));
        Ok(
            json!({"occupied_minor":occupied,"available_minor":available,"needs_review":stale,"overallocated":overallocated,"fact_conflict":fact_conflict,"pending":pending,"links":links,"next_actions":if pending{vec!["Read linked transactions; revise and rebind current evidence atomically or keep unmatched"]}else{vec![]}}),
        )
    }
    pub(super) async fn transaction_quality(
        &mut self,
        t: &CurrentTransaction,
    ) -> Result<Vec<String>> {
        if t.state != "posted" {
            return Ok(vec![]);
        }
        let mut flags = quality(&t.transaction)
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        if let Movement::Refund {
            original_transaction_id,
            refund_allocations,
            ..
        } = &t.transaction.movement
        {
            let original = self.get_current(*original_transaction_id).await?;
            if refund_allocations.iter().any(|r| {
                original
                    .transaction
                    .allocations
                    .iter()
                    .any(|a| a.allocation_id == Some(r.allocation_id) && a.category_id.is_none())
            }) {
                flags.push("uncategorized".into());
            }
        }
        let links = self.evidence_links(Some(t.id), true).await?;
        let legs = self.funding_legs(t.id).await?;
        let mut coverage = BTreeMap::<(Uuid, String, NaiveDate, Uuid), Amount>::new();
        for link in &links {
            let o = self.observation(link.observation_id).await?;
            if link.observation_version != o.current_version || !self.source_current(&o).await? {
                flags.push("evidence_needs_review".into());
                continue;
            }
            if let (
                "movement",
                ObservationPayload::Movement {
                    account_id: Some(a),
                    money: Some(m),
                    posted_on: Some(date),
                    ..
                },
            ) = (link.role.as_str(), &o.payload)
            {
                let total = coverage
                    .entry((*a, m.currency.clone(), *date, o.source_id))
                    .or_default();
                *total = total.checked_add(link.amount_minor.ok_or(Error::NotFound)?)?;
            }
        }
        let missing = legs.iter().any(|leg| {
            !coverage.iter().any(|((a, c, d, _), amount)| {
                *a == leg.account_id
                    && c == &leg.currency
                    && *d == leg.posted_on
                    && *amount >= absolute(leg.amount_minor).unwrap_or(leg.amount_minor)
            })
        });
        if missing || links.is_empty() {
            flags.push("evidence_missing".into());
        }
        if !t.transaction.inferred_posting_accounts.is_empty() {
            flags.push("posting_date_inferred".into());
        }
        flags.sort();
        flags.dedup();
        Ok(flags)
    }
}
