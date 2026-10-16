use super::imports::Observed;
use super::*;

impl Store {
    pub async fn reconcile(&self, p: &Principal, request: ReconcileRequest) -> Result<Value> {
        p.require("sumi:read")?;
        let mut s = self
            .session_mode(p, request.ledger_id, true, "sumi:read")
            .await?;
        let observation = s.observation(request.statement_observation_id).await?;
        version(request.statement_version, observation.current_version)?;
        let statement = if let ObservationPayload::Statement { statement } = &observation.payload {
            statement
        } else {
            return Err(Error::Invalid(
                "Reconcile requires a statement observation".into(),
            ));
        };
        require(
            statement.account_id == request.account_id
                && statement.currency == request.currency
                && statement.period_start == request.period_start
                && statement.period_end == request.period_end,
            "Reconciliation scope must exactly match statement scope",
        )?;
        let scope_hash = hash(&request)?;
        let existing:Option<Value>=sqlx::query_scalar("SELECT result FROM reconciliations WHERE ledger_id=$1 AND scope_hash=$2 AND ledger_revision=$3").bind(s.ledger_id).bind(&scope_hash).bind(s.revision).fetch_optional(&mut *s.tx).await?;
        if let Some(v) = existing {
            s.tx.commit().await?;
            return Ok(v);
        }
        let data = s.reconcile_statement(&observation, statement).await?;
        let response = envelope(data, s.revision);
        sqlx::query("INSERT INTO reconciliations(ledger_id,id,ledger_revision,account_id,statement_observation_id,statement_version,scope_hash,result) VALUES($1,$2,$3,$4,$5,$6,$7,$8)").bind(s.ledger_id).bind(Uuid::new_v4()).bind(s.revision).bind(request.account_id).bind(observation.id).bind(observation.current_version).bind(scope_hash).bind(&response).execute(&mut *s.tx).await?;
        s.tx.commit().await?;
        Ok(response)
    }
}
fn magnitude(amount: Amount) -> Result<Amount> {
    if amount < Amount::ZERO {
        amount.checked_neg()
    } else {
        Ok(amount)
    }
}
fn difference(a: Amount, b: Amount) -> Result<Amount> {
    a.checked_add(b.checked_neg()?)
}
fn control_count(expected: Option<&ControlCount>, actual: usize) -> Value {
    match expected {
        Some(c) => {
            json!({"status":if u64::from(c.value)==actual as u64{"matched"}else{"mismatch"},"expected":c.value,"actual":actual,"locator":c.locator})
        }
        None => json!({"status":"unavailable","actual":actual}),
    }
}
fn control_amount(expected: Option<&ControlAmount>, actual: Amount) -> Value {
    match expected {
        Some(c) => {
            json!({"status":if c.value==actual{"matched"}else{"mismatch"},"expected":c.value,"actual":actual,"locator":c.locator})
        }
        None => json!({"status":"unavailable","actual":actual}),
    }
}
pub(super) fn source_location(body: &str, locator: &str) -> Option<Value> {
    if locator == "$" {
        return Some(serde_json::from_str(body).unwrap_or_else(|_| json!(body)));
    }
    if locator.starts_with('/') {
        return serde_json::from_str::<Value>(body)
            .ok()?
            .pointer(locator)
            .cloned();
    }
    if let Some(line) = locator
        .strip_prefix("line:")
        .and_then(|n| n.parse::<usize>().ok())
    {
        return line
            .checked_sub(1)
            .and_then(|i| body.lines().nth(i))
            .map(|l| json!(l));
    }
    None
}
fn verifies_number(value: Value, expected: i128, precision: Option<u8>) -> bool {
    use std::str::FromStr;
    let text = match value {
        Value::String(s) => s,
        Value::Number(n) => n.to_string(),
        _ => return false,
    };
    text.split(|c: char| !c.is_ascii_digit() && !matches!(c, '.' | '+' | '-' | ',' | 'e' | 'E'))
        .filter(|s| !s.is_empty())
        .any(|token| {
            if token.len() > 100 || token.contains(['e', 'E']) {
                return false;
            }
            if token.contains(',') {
                let integer = token
                    .trim_start_matches(['+', '-'])
                    .split('.')
                    .next()
                    .unwrap_or_default();
                let groups = integer.split(',').collect::<Vec<_>>();
                if groups[0].is_empty()
                    || groups[0].len() > 3
                    || groups.iter().skip(1).any(|g| g.len() != 3)
                {
                    return false;
                }
            }
            let token = token.replace(',', "");
            let Ok(value) = bigdecimal::BigDecimal::from_str(&token) else {
                return false;
            };
            let expected = bigdecimal::BigDecimal::from(expected);
            value == expected
                || precision.is_some_and(|p| {
                    value * bigdecimal::BigDecimal::from(10_i128.pow(p as u32)) == expected
                })
        })
}
impl Session {
    pub(super) async fn reconcile_statement(
        &mut self,
        statement_observation: &Observed,
        s: &Statement,
    ) -> Result<Value> {
        let start = s
            .period_start
            .pred_opt()
            .ok_or_else(|| Error::Invalid("Statement date out of range".into()))?;
        let end = s
            .period_end
            .pred_opt()
            .ok_or_else(|| Error::Invalid("Statement date out of range".into()))?;
        let openings = self.openings().await?;
        let baseline = openings
            .get(&s.account_id)
            .filter(|(_, as_of)| *as_of < s.period_start);
        let opening = self
            .balances(Some(start))
            .await?
            .get(&s.account_id)
            .copied()
            .unwrap_or_default();
        let closing = self
            .balances(Some(end))
            .await?
            .get(&s.account_id)
            .copied()
            .unwrap_or_default();
        let opening_difference = s
            .opening_balance
            .map(|v| difference(opening, v))
            .transpose()?;
        let closing_difference = s
            .closing_balance
            .map(|v| difference(closing, v))
            .transpose()?;
        let source_current = self.source_current(statement_observation).await?;
        let mut uncertain = vec![];
        let mut incomplete_extractions = false;
        let mut rows = vec![];
        let mut debit = Amount::ZERO;
        let mut credit = Amount::ZERO;
        for o in self
            .observations()
            .await?
            .into_iter()
            .filter(|o| o.source_version_id == statement_observation.source_version_id)
        {
            if let ObservationPayload::Movement {
                account_id,
                money,
                direction,
                posted_on,
                posting_date_inferred,
            } = &o.payload
            {
                if account_id.is_some_and(|id| id != s.account_id)
                    || money.as_ref().is_some_and(|m| m.currency != s.currency)
                {
                    continue;
                }
                if posted_on.is_some_and(|d| d < s.period_start || d >= s.period_end) {
                    continue;
                }
                if let (Some(_), Some(money), Some(direction), Some(_)) =
                    (account_id, money, direction, posted_on)
                {
                    if *direction == Direction::In {
                        credit = credit.checked_add(money.amount_minor)?;
                    } else {
                        debit = debit.checked_add(money.amount_minor)?;
                    }
                    if *posting_date_inferred {
                        uncertain.push(o.id);
                    }
                    rows.push(o);
                } else {
                    incomplete_extractions = true;
                    uncertain.push(o.id);
                    rows.push(o);
                }
            }
        }
        let mut legs = vec![];
        for t in self.current_transactions().await?.into_iter().filter(|t| {
            t.state == "posted" && !matches!(t.transaction.movement, Movement::Opening { .. })
        }) {
            let scoped = self
                .funding_legs(t.id)
                .await?
                .into_iter()
                .filter(|l| {
                    l.account_id == s.account_id
                        && l.currency == s.currency
                        && l.posted_on >= s.period_start
                        && l.posted_on < s.period_end
                })
                .collect::<Vec<_>>();
            if !scoped.is_empty()
                && t.transaction
                    .inferred_posting_accounts
                    .contains(&s.account_id)
            {
                uncertain.push(t.id);
            }
            legs.extend(scoped);
        }
        let links = self.evidence_links(None, true).await?;
        let mut unmatched_rows = vec![];
        let mut matched_legs = BTreeMap::<(Uuid, Uuid, NaiveDate), Amount>::new();
        let mut stale = false;
        for o in &rows {
            let mut matched = Amount::ZERO;
            for link in links
                .iter()
                .filter(|l| l.observation_id == o.id && l.role == "movement")
            {
                if link.observation_version != o.current_version || !source_current {
                    stale = true;
                    continue;
                }
                if let ObservationPayload::Movement {
                    money: Some(m),
                    direction: Some(d),
                    posted_on: Some(date),
                    ..
                } = &o.payload
                    && let Some(leg) = legs.iter().find(|l| {
                        l.transaction_id == link.transaction_id
                            && l.account_id == s.account_id
                            && l.currency == m.currency
                            && l.posted_on == *date
                            && ((l.amount_minor > Amount::ZERO) == (*d == Direction::In))
                    })
                {
                    let amount = link.amount_minor.ok_or(Error::NotFound)?;
                    matched = matched.checked_add(amount)?;
                    let total = matched_legs
                        .entry((leg.transaction_id, leg.account_id, leg.posted_on))
                        .or_default();
                    *total = total.checked_add(amount)?;
                }
            }
            let amount = if let ObservationPayload::Movement { money: Some(m), .. } = &o.payload {
                Some(m.amount_minor)
            } else {
                None
            };
            if amount.is_none_or(|a| a != matched) {
                unmatched_rows.push(json!({"observation_id":o.id,"version":o.current_version,"expected_minor":amount,"matched_minor":matched,"record_locator":o.record_locator}));
            }
        }
        let mut unmatched_legs = vec![];
        for leg in &legs {
            let matched = matched_legs
                .get(&(leg.transaction_id, leg.account_id, leg.posted_on))
                .copied()
                .unwrap_or_default();
            if matched != magnitude(leg.amount_minor)? {
                unmatched_legs.push(json!({"leg":leg,"matched_minor":matched}));
            }
        }
        let body: String =
            sqlx::query_scalar("SELECT body FROM source_versions WHERE ledger_id=$1 AND id=$2")
                .bind(self.ledger_id)
                .bind(statement_observation.source_version_id)
                .fetch_one(&mut *self.tx)
                .await?;
        let actual_locators = rows
            .iter()
            .map(|o| o.record_locator.clone())
            .collect::<std::collections::BTreeSet<_>>();
        let requested_locators = s
            .record_locators
            .as_ref()
            .map(|l| l.iter().cloned().collect::<std::collections::BTreeSet<_>>());
        let location = s
            .range_locator
            .as_ref()
            .and_then(|l| source_location(&body, l));
        let manifest = location
            .as_ref()
            .and_then(|v| v.as_array())
            .and_then(|values| {
                values
                    .iter()
                    .map(|v| {
                        v.as_str()
                            .map(str::to_owned)
                            .or_else(|| v.get("locator").and_then(Value::as_str).map(str::to_owned))
                    })
                    .collect::<Option<std::collections::BTreeSet<_>>>()
            });
        let manifest_proven = manifest
            .as_ref()
            .zip(requested_locators.as_ref())
            .is_some_and(|(manifest, requested)| {
                manifest == requested && requested == &actual_locators
            });
        let range_mismatch = requested_locators
            .as_ref()
            .is_some_and(|expected| expected != &actual_locators)
            || manifest
                .as_ref()
                .zip(requested_locators.as_ref())
                .is_some_and(|(manifest, requested)| manifest != requested);
        let controls = json!({"count":control_count(s.expected_count.as_ref(),rows.len()),"debit_total":control_amount(s.expected_debit_total.as_ref(),debit),"credit_total":control_amount(s.expected_credit_total.as_ref(),credit)});
        let control_mismatch = controls
            .as_object()
            .ok_or(Error::NotFound)?
            .values()
            .any(|v| v["status"] == "mismatch");
        let mut verified_controls = 0;
        let mut supplied_controls = 0;
        if let Some(c) = &s.expected_count {
            supplied_controls += 1;
            if source_location(&body, &c.locator)
                .is_some_and(|v| verifies_number(v, i128::from(c.value), None))
            {
                verified_controls += 1;
            }
        }
        for c in [&s.expected_debit_total, &s.expected_credit_total]
            .into_iter()
            .flatten()
        {
            supplied_controls += 1;
            if source_location(&body, &c.locator).is_some_and(|v| {
                verifies_number(v, c.value.value(), currency_precision(&s.currency).ok())
            }) {
                verified_controls += 1;
            }
        }
        let unfinished_batch:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM import_batches b JOIN batch_sources bs ON bs.ledger_id=b.ledger_id AND bs.batch_id=b.id WHERE b.ledger_id=$1 AND bs.source_version_id=$2 AND NOT b.complete)").bind(self.ledger_id).bind(statement_observation.source_version_id).fetch_one(&mut *self.tx).await?;
        let known_incomplete = s.source_coverage == Coverage::Partial
            || !s.extraction_complete
            || !s.known_gaps.is_empty()
            || control_mismatch
            || range_mismatch
            || unfinished_batch
            || incomplete_extractions
            || !source_current;
        let control_proven = supplied_controls > 0
            && verified_controls == supplied_controls
            && location.is_some()
            && !control_mismatch;
        let completeness = if known_incomplete {
            "incomplete"
        } else if s.source_coverage == Coverage::Complete && (manifest_proven || control_proven) {
            "complete"
        } else {
            "unknown"
        };
        let explicit_empty = (manifest_proven && actual_locators.is_empty())
            || (s.expected_count.as_ref().is_some_and(|c| c.value == 0) && control_proven);
        let missing_balance =
            baseline.is_none() || opening_difference.is_none() || closing_difference.is_none();
        let reconciliation = if missing_balance || !uncertain.is_empty() || !source_current {
            "incomplete"
        } else if opening_difference != Some(Amount::ZERO)
            || closing_difference != Some(Amount::ZERO)
        {
            "mismatch"
        } else if stale
            || !unmatched_rows.is_empty()
            || !unmatched_legs.is_empty()
            || (rows.is_empty() && !explicit_empty)
        {
            "balance_matched"
        } else {
            "reconciled"
        };
        let scope = json!({"account_id":s.account_id,"currency":s.currency,"period_start":s.period_start,"period_end":s.period_end,"source_version_id":statement_observation.source_version_id,"statement_observation_id":statement_observation.id,"statement_version":statement_observation.current_version});
        Ok(
            json!({"scope":scope,"reconciliation":{"status":reconciliation,"scope":scope,"ledger_revision":self.revision,"stale":false,"opening_minor":opening,"movement_minor":difference(closing,opening)?,"closing_minor":closing,"opening_difference_minor":opening_difference,"closing_difference_minor":closing_difference,"opening_baseline_confirmed":baseline.is_some(),"unmatched_observations":unmatched_rows,"unmatched_ledger_legs":unmatched_legs,"uncertain_dates_or_fields":uncertain,"acquired_rows":rows.len(),"explicit_empty_statement":explicit_empty},"import_completeness":{"status":completeness,"scope":scope,"ledger_revision":self.revision,"stale":false,"source_coverage":s.source_coverage,"extraction_complete":s.extraction_complete,"known_gaps":s.known_gaps,"controls":controls,"actual_count":rows.len(),"actual_debit_minor":debit,"actual_credit_minor":credit,"record_manifest_verified":manifest_proven,"control_provenance_verified":control_proven,"record_range_mismatch":range_mismatch,"unfinished_batch":unfinished_batch,"limitations":"Only the stated source and scope are checked; saved content authenticity is submitter-declared"}}),
        )
    }
}
