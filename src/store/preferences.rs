use super::*;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub(super) struct PreferenceData {
    #[serde(default)]
    merchant_aliases: BTreeMap<String, String>,
    #[serde(default)]
    classification_rules: Vec<ClassificationRule>,
}
impl Store {
    pub async fn preferences_update(
        &self,
        principal: &Principal,
        request: PreferencesUpdate,
    ) -> Result<Value> {
        let raw = serde_json::to_value(&request)?;
        let fingerprint = hash(&raw)?;
        let tool = "sumi_preferences_update";
        let mut s = self.session(principal, request.ledger_id, true).await?;
        if let Some(response) = s
            .replay(principal, tool, &request.idempotency_key, &fingerprint)
            .await?
        {
            return Ok(response);
        }
        require(
            !request.reason.trim().is_empty(),
            "Preference updates require a reason",
        )?;
        let (old_version, old_prefs) = s.preference_data().await?;
        version(request.expected_version, old_version)?;
        let before = s.preferences_snapshot().await?;
        let mut data = old_prefs.clone();
        let mut changed = false;
        let mut categories = s.categories().await?;
        let mut tags = s.tags().await?;
        if let Some(inputs) = request.categories {
            require(inputs.len() <= 100, "At most 100 category updates")?;
            for input in inputs {
                require(
                    !input.name.trim().is_empty() && input.name.len() <= 256,
                    "Invalid category name",
                )?;
                let matching = categories.values().find(|c| {
                    c.name == input.name && c.kind == input.kind && c.parent_id == input.parent_id
                });
                if input.id.is_none()
                    && let Some(existing) = matching
                {
                    require(
                        existing.active == input.active,
                        "Use category id and version to change an existing category",
                    )?;
                }
                let id = input
                    .id
                    .or_else(|| matching.map(|c| c.id))
                    .unwrap_or_else(Uuid::new_v4);
                let old = categories.get(&id);
                if input.id.is_some() {
                    let old = old.ok_or(Error::NotFound)?;
                    version(
                        input.expected_version.ok_or_else(|| {
                            Error::Invalid("Category expected_version is required".into())
                        })?,
                        old.version,
                    )?;
                } else {
                    require(
                        input.expected_version.is_none(),
                        "New category has no expected_version",
                    )?;
                }
                let mut category = Category {
                    id,
                    name: input.name,
                    kind: input.kind,
                    parent_id: input.parent_id,
                    active: input.active,
                    version: old.map_or(1, |o| o.version),
                };
                if old != Some(&category) {
                    if old.is_some() {
                        category.version =
                            category.version.checked_add(1).ok_or(Error::Overflow)?;
                    }
                    sqlx::query("INSERT INTO categories(ledger_id,id,name,kind,parent_id,active,version) VALUES($1,$2,$3,$4,$5,$6,$7) ON CONFLICT(ledger_id,id) DO UPDATE SET name=EXCLUDED.name,kind=EXCLUDED.kind,parent_id=EXCLUDED.parent_id,active=EXCLUDED.active,version=EXCLUDED.version")
                        .bind(s.ledger_id).bind(id).bind(&category.name).bind(category_kind(&category.kind)).bind(category.parent_id).bind(category.active).bind(category.version).execute(&mut *s.tx).await?;
                    categories.insert(id, category);
                    changed = true;
                }
            }
        }
        for category in categories.values() {
            let mut visited = std::collections::BTreeSet::from([category.id]);
            let mut parent = category.parent_id;
            while let Some(id) = parent {
                require(visited.insert(id), "Category hierarchy contains a cycle")?;
                let ancestor = categories.get(&id).ok_or(Error::NotFound)?;
                require(
                    ancestor.kind == category.kind,
                    "Category parent must have the same income/expense kind",
                )?;
                parent = ancestor.parent_id;
            }
        }
        if let Some(inputs) = request.tags {
            require(inputs.len() <= 100, "At most 100 tag updates")?;
            for input in inputs {
                require(
                    !input.name.trim().is_empty() && input.name.len() <= 256,
                    "Invalid tag name",
                )?;
                let matching = tags.values().find(|t| t.name == input.name);
                if input.id.is_none()
                    && let Some(existing) = matching
                {
                    require(
                        existing.active == input.active,
                        "Use tag id and version to change an existing tag",
                    )?;
                }
                let id = input
                    .id
                    .or_else(|| matching.map(|t| t.id))
                    .unwrap_or_else(Uuid::new_v4);
                let old = tags.get(&id);
                if input.id.is_some() {
                    let old = old.ok_or(Error::NotFound)?;
                    version(
                        input.expected_version.ok_or_else(|| {
                            Error::Invalid("Tag expected_version is required".into())
                        })?,
                        old.version,
                    )?;
                } else {
                    require(
                        input.expected_version.is_none(),
                        "New tag has no expected_version",
                    )?;
                }
                let mut tag = Tag {
                    id,
                    name: input.name,
                    active: input.active,
                    version: old.map_or(1, |o| o.version),
                };
                if old != Some(&tag) {
                    if old.is_some() {
                        tag.version = tag.version.checked_add(1).ok_or(Error::Overflow)?;
                    }
                    sqlx::query("INSERT INTO tags(ledger_id,id,name,active,version) VALUES($1,$2,$3,$4,$5) ON CONFLICT(ledger_id,id) DO UPDATE SET name=EXCLUDED.name,active=EXCLUDED.active,version=EXCLUDED.version")
                        .bind(s.ledger_id).bind(id).bind(&tag.name).bind(tag.active).bind(tag.version).execute(&mut *s.tx).await?;
                    tags.insert(id, tag);
                    changed = true;
                }
            }
        }
        if let Some(aliases) = request.merchant_aliases {
            require(
                aliases.len() <= 1000
                    && aliases.iter().all(|(a, b)| {
                        !a.is_empty() && a.len() <= 512 && !b.is_empty() && b.len() <= 512
                    }),
                "Invalid merchant aliases",
            )?;
            data.merchant_aliases = aliases;
        }
        if let Some(rules) = request.classification_rules {
            require(rules.len() <= 100, "At most 100 classification rules")?;
            data.classification_rules = rules;
        }
        for rule in &data.classification_rules {
            require(
                !rule.merchant_contains.is_empty() && rule.merchant_contains.len() <= 512,
                "Invalid rule merchant pattern",
            )?;
            let category = categories.get(&rule.category_id).ok_or(Error::NotFound)?;
            require(
                category.active && category.kind == rule.kind,
                "Rule category must be active with matching kind",
            )?;
            for id in &rule.tags {
                require(
                    tags.get(id).is_some_and(|t| t.active),
                    "Rule tag must be active in this ledger",
                )?;
            }
        }
        let row = sqlx::query("SELECT reporting_currency,timezone FROM ledgers WHERE id=$1")
            .bind(s.ledger_id)
            .fetch_one(&mut *s.tx)
            .await?;
        let old_currency: String = row.try_get("reporting_currency")?;
        let old_timezone: String = row.try_get("timezone")?;
        let currency = request.reporting_currency.unwrap_or(old_currency.clone());
        currency_precision(&currency)?;
        let timezone = request.timezone.unwrap_or(old_timezone.clone());
        timezone
            .parse::<chrono_tz::Tz>()
            .map_err(|_| Error::Invalid("Unknown IANA timezone".into()))?;
        if timezone != old_timezone {
            let used: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM journal_entries WHERE ledger_id=$1)",
            )
            .bind(s.ledger_id)
            .fetch_one(&mut *s.tx)
            .await?;
            require(!used, "Timezone is frozen after the first journal entry")?;
        }
        changed |= data != old_prefs || currency != old_currency || timezone != old_timezone;
        s.validate_classifications(&categories, &tags).await?;
        if changed {
            let next = old_version.checked_add(1).ok_or(Error::Overflow)?;
            sqlx::query("INSERT INTO preferences(ledger_id,version,data) VALUES($1,$2,$3) ON CONFLICT(ledger_id) DO UPDATE SET version=EXCLUDED.version,data=EXCLUDED.data").bind(s.ledger_id).bind(next).bind(serde_json::to_value(&data)?).execute(&mut *s.tx).await?;
            sqlx::query("UPDATE ledgers SET reporting_currency=$2,timezone=$3 WHERE id=$1")
                .bind(s.ledger_id)
                .bind(currency)
                .bind(timezone)
                .execute(&mut *s.tx)
                .await?;
            s.bump().await?;
        }
        let response = envelope(
            json!({"preferences":s.preferences_snapshot().await?,"previous":before}),
            s.revision,
        );
        s.finish(
            WriteOperation {
                principal,
                tool,
                key: &request.idempotency_key,
                request_hash: &fingerprint,
                request: raw,
            },
            response,
            changed,
            Uuid::new_v4(),
        )
        .await
    }
}
fn category_kind(kind: &CategoryKind) -> &'static str {
    match kind {
        CategoryKind::Income => "income",
        CategoryKind::Expense => "expense",
    }
}
impl Session {
    pub(super) async fn preference_data(&mut self) -> Result<(i64, PreferenceData)> {
        let row = sqlx::query("SELECT version,data FROM preferences WHERE ledger_id=$1")
            .bind(self.ledger_id)
            .fetch_optional(&mut *self.tx)
            .await?;
        match row {
            Some(r) => Ok((
                r.try_get("version")?,
                serde_json::from_value(r.try_get("data")?)?,
            )),
            None => Ok((1, PreferenceData::default())),
        }
    }
    pub(super) async fn categories(&mut self) -> Result<BTreeMap<Uuid, Category>> {
        let values: Vec<Value> = sqlx::query_scalar(
            "SELECT to_jsonb(c)-'ledger_id' FROM categories c WHERE ledger_id=$1 ORDER BY id",
        )
        .bind(self.ledger_id)
        .fetch_all(&mut *self.tx)
        .await?;
        values
            .into_iter()
            .map(|v| {
                let c: Category = serde_json::from_value(v)?;
                Ok((c.id, c))
            })
            .collect()
    }
    pub(super) async fn tags(&mut self) -> Result<BTreeMap<Uuid, Tag>> {
        let values: Vec<Value> = sqlx::query_scalar(
            "SELECT to_jsonb(t)-'ledger_id' FROM tags t WHERE ledger_id=$1 ORDER BY id",
        )
        .bind(self.ledger_id)
        .fetch_all(&mut *self.tx)
        .await?;
        values
            .into_iter()
            .map(|v| {
                let t: Tag = serde_json::from_value(v)?;
                Ok((t.id, t))
            })
            .collect()
    }
    pub(super) async fn preferences_snapshot(&mut self) -> Result<Value> {
        let (version, data) = self.preference_data().await?;
        let categories = self.categories().await?;
        let tags = self.tags().await?;
        Ok(
            json!({"version":version,"merchant_aliases":data.merchant_aliases,"classification_rules":data.classification_rules,"categories":categories.values().collect::<Vec<_>>(),"tags":tags.values().collect::<Vec<_>>() }),
        )
    }
    pub(super) async fn validate_classifications(
        &mut self,
        categories: &BTreeMap<Uuid, Category>,
        tags: &BTreeMap<Uuid, Tag>,
    ) -> Result<()> {
        for t in self
            .current_transactions()
            .await?
            .into_iter()
            .filter(|t| t.state == "posted")
        {
            validate_allocations(&t.transaction, categories, tags, false)?;
        }
        Ok(())
    }
    pub(super) async fn normalize_transaction(
        &mut self,
        id: Uuid,
        data: &mut TransactionData,
        old: Option<&TransactionData>,
    ) -> Result<()> {
        let (_, preferences) = self.preference_data().await?;
        if let Some(period) = &data.occurred_period {
            require(
                data.occurred_on.is_none() && data.occurred_at.is_none(),
                "Use occurred_period only when the exact day is unknown",
            )?;
            let date = match period.len() {
                4 => format!("{period}-01-01"),
                7 => format!("{period}-01"),
                _ => {
                    return Err(Error::Invalid(
                        "occurred_period must be YYYY or YYYY-MM".into(),
                    ));
                }
            };
            NaiveDate::parse_from_str(&date, "%Y-%m-%d")
                .map_err(|_| Error::Invalid("Invalid occurrence period".into()))?;
        }
        if let Some(timestamp) = data.occurred_at {
            let timezone: String = sqlx::query_scalar("SELECT timezone FROM ledgers WHERE id=$1")
                .bind(self.ledger_id)
                .fetch_one(&mut *self.tx)
                .await?;
            let timezone = timezone
                .parse::<chrono_tz::Tz>()
                .map_err(|_| Error::Invalid("Invalid ledger timezone".into()))?;
            let date = timestamp.with_timezone(&timezone).date_naive();
            require(
                data.occurred_on.is_none_or(|d| d == date),
                "Occurrence timestamp and date disagree in the ledger timezone",
            )?;
            data.occurred_on = Some(date);
        }
        let categories = self.categories().await?;
        let tags = self.tags().await?;
        normalize_allocations(id, data)?;
        if data.normalized_merchant.is_none() {
            data.normalized_merchant = data
                .merchant
                .as_ref()
                .and_then(|m| preferences.merchant_aliases.get(m).cloned());
        }
        // Ordinary imports only fill unclassified allocations. Explicit replacement remains audited.
        let merchant = data
            .normalized_merchant
            .as_ref()
            .or(data.merchant.as_ref())
            .cloned()
            .unwrap_or_default();
        let kind = match data.movement {
            Movement::Income { .. } => Some(CategoryKind::Income),
            Movement::Expense { .. } => Some(CategoryKind::Expense),
            _ => None,
        };
        if let Some(kind) = kind
            && let Some(rule) = preferences
                .classification_rules
                .iter()
                .find(|r| r.kind == kind && merchant.contains(&r.merchant_contains))
        {
            for a in &mut data.allocations {
                if a.category_id.is_none() {
                    a.category_id = Some(rule.category_id);
                    a.tags.extend(&rule.tags);
                    a.tags.sort();
                    a.tags.dedup();
                }
            }
        }
        validate_allocations(data, &categories, &tags, true)?;
        if let Some(original) = &data.original_money {
            original.validate()?;
            original.amount_minor.positive()?;
        }
        let settlements = data
            .settlement
            .iter()
            .chain(data.fees.iter().filter_map(|f| f.settlement.as_ref()));
        for settlement in settlements {
            settlement.money.validate()?;
            settlement.money.amount_minor.positive()?;
            self.require_source_version(settlement.source_version_id)
                .await?;
            require(
                !settlement.record_locator.is_empty(),
                "Settlement needs source locator",
            )?;
        }
        let historical_tags = old
            .map(|o| {
                o.tags
                    .iter()
                    .copied()
                    .chain(
                        o.allocations
                            .iter()
                            .chain(o.fees.iter().flat_map(|f| &f.allocations))
                            .flat_map(|a| a.tags.iter().copied()),
                    )
                    .collect::<std::collections::BTreeSet<_>>()
            })
            .unwrap_or_default();
        for id in data.tags.iter().chain(
            data.allocations
                .iter()
                .chain(data.fees.iter().flat_map(|f| &f.allocations))
                .flat_map(|a| &a.tags),
        ) {
            require(
                tags.get(id).is_some_and(|t| t.active) || historical_tags.contains(id),
                "Cannot newly assign an inactive tag",
            )?;
        }
        // Preserving an inactive category/tag on existing records is allowed.
        for a in data
            .allocations
            .iter()
            .chain(data.fees.iter().flat_map(|f| &f.allocations))
        {
            if let Some(cid) = a.category_id {
                let historical = old.is_some_and(|o| {
                    o.allocations
                        .iter()
                        .chain(o.fees.iter().flat_map(|f| &f.allocations))
                        .any(|old| {
                            old.allocation_id == a.allocation_id && old.category_id == Some(cid)
                        })
                });
                require(
                    categories.get(&cid).is_some_and(|c| c.active) || historical,
                    "Cannot newly assign an inactive category",
                )?;
            }
        }
        Ok(())
    }
}
pub(super) fn normalize_allocations(id: Uuid, data: &mut TransactionData) -> Result<()> {
    require(
        data.allocations.len() <= 100
            && data.fees.len() <= 20
            && data.tags.len() <= 100
            && data.identifiers.len() <= 50,
        "Transaction details exceed limits",
    )?;
    fn component(id: Uuid, allocations: &mut Vec<Allocation>, money: &Money) -> Result<()> {
        if allocations.is_empty() {
            allocations.push(Allocation {
                allocation_id: Some(Uuid::new_v5(&id, b"uncategorized")),
                category_id: None,
                amount_minor: money.amount_minor,
                tags: vec![],
            });
        }
        let mut sum = Amount::ZERO;
        let mut seen = std::collections::BTreeSet::new();
        for (i, a) in allocations.iter_mut().enumerate() {
            a.amount_minor.positive()?;
            if a.allocation_id.is_none() {
                a.allocation_id = Some(Uuid::new_v5(&id, format!("allocation:{i}").as_bytes()));
            }
            require(seen.insert(a.allocation_id), "Duplicate allocation ID")?;
            sum = sum.checked_add(a.amount_minor)?;
            a.tags.sort();
            a.tags.dedup();
        }
        require(
            sum == money.amount_minor,
            "Allocation amounts must equal the component amount",
        )
    }
    match &data.movement {
        Movement::Expense { money, .. } | Movement::Income { money, .. } => {
            component(id, &mut data.allocations, money)?
        }
        _ => require(
            data.allocations.is_empty(),
            "Only income and expense principals accept allocations",
        )?,
    }
    let mut fee_ids = std::collections::BTreeSet::new();
    for (i, f) in data.fees.iter_mut().enumerate() {
        let fid = *f
            .fee_id
            .get_or_insert_with(|| Uuid::new_v5(&id, format!("fee:{i}").as_bytes()));
        require(fee_ids.insert(fid), "Duplicate fee ID")?;
        component(fid, &mut f.allocations, &f.money)?;
    }
    data.tags.sort();
    data.tags.dedup();
    data.identifiers.sort_by_key(|i| {
        (
            i.namespace.clone(),
            i.account_id,
            i.event_type.clone(),
            i.external_id.clone(),
        )
    });
    data.identifiers.dedup();
    Ok(())
}
fn validate_allocations(
    data: &TransactionData,
    categories: &BTreeMap<Uuid, Category>,
    tags: &BTreeMap<Uuid, Tag>,
    _strict: bool,
) -> Result<()> {
    let principal_kind = if matches!(data.movement, Movement::Income { .. }) {
        CategoryKind::Income
    } else {
        CategoryKind::Expense
    };
    for (allocations, kind) in std::iter::once((&data.allocations, &principal_kind)).chain(
        data.fees
            .iter()
            .map(|f| (&f.allocations, &CategoryKind::Expense)),
    ) {
        for a in allocations {
            if let Some(cid) = a.category_id {
                let c = categories.get(&cid).ok_or(Error::NotFound)?;
                require(c.kind == *kind, "Category kind differs from component kind")?;
            }
            for tag in &a.tags {
                require(tags.contains_key(tag), "Tag is outside this ledger")?;
            }
        }
    }
    for tag in &data.tags {
        require(tags.contains_key(tag), "Tag is outside this ledger")?;
    }
    Ok(())
}
