use super::*;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Observed {
    pub id: Uuid,
    pub source_id: Uuid,
    pub kind: String,
    pub version: i64,
    pub current_version: i64,
    pub source_version_id: Uuid,
    pub record_locator: String,
    pub payload: ObservationPayload,
    pub references: Vec<String>,
    pub extractor: String,
    pub review_note: Option<String>,
    pub ignored_reason: Option<String>,
}
fn rule(code: &'static str, message: impl Into<String>, entities: Value) -> Error {
    Error::Rule {
        code,
        message: message.into(),
        entities,
    }
}

impl Store {
    pub async fn sources_ingest(&self, p: &Principal, request: SourcesIngest) -> Result<Value> {
        require(
            !request.sources.is_empty() && request.sources.len() <= 100,
            "Submit 1..100 sources",
        )?;
        let raw = serde_json::to_value(&request)?;
        let fingerprint = hash(&raw)?;
        let tool = "sumi_sources_ingest";
        let mut s = self.session(p, request.ledger_id, true).await?;
        if let Some(v) = s
            .replay(p, tool, &request.idempotency_key, &fingerprint)
            .await?
        {
            return Ok(v);
        }
        let operation_id = Uuid::new_v4();
        let mut changed = false;
        let mut results = vec![];
        for source in request.sources {
            for value in [&source.provider, &source.source_scope, &source.external_id] {
                require(
                    !value.is_empty() && value.len() <= 1024,
                    "Source identity fields require 1..1024 bytes",
                )?;
            }
            require(
                source.body.len() <= 512 * 1024,
                "Source body exceeds 512 KiB; split the material",
            )?;
            require(
                source.origin_uri.as_ref().is_none_or(|u| u.len() <= 4096),
                "Source URI is too long",
            )?;
            let content_hash = format!("{:x}", Sha256::digest(source.body.as_bytes()));
            let capture = serde_json::to_value(source.capture_kind)?
                .as_str()
                .ok_or(Error::NotFound)?
                .to_owned();
            let row=sqlx::query("SELECT id FROM sources WHERE ledger_id=$1 AND provider=$2 AND source_scope=$3 AND external_id=$4").bind(s.ledger_id).bind(&source.provider).bind(&source.source_scope).bind(&source.external_id).fetch_optional(&mut *s.tx).await?;
            let source_id = if let Some(row) = row {
                row.try_get("id")?
            } else {
                let id = Uuid::new_v4();
                sqlx::query("INSERT INTO sources(ledger_id,id,provider,source_scope,external_id) VALUES($1,$2,$3,$4,$5)").bind(s.ledger_id).bind(id).bind(&source.provider).bind(&source.source_scope).bind(&source.external_id).execute(&mut *s.tx).await?;
                id
            };
            let existing=sqlx::query("SELECT id,version FROM source_versions WHERE ledger_id=$1 AND source_id=$2 AND content_hash=$3 AND capture_kind=$4 AND origin_uri IS NOT DISTINCT FROM $5").bind(s.ledger_id).bind(source_id).bind(&content_hash).bind(&capture).bind(&source.origin_uri).fetch_optional(&mut *s.tx).await?;
            if let Some(row) = existing {
                results.push(json!({"source_id":source_id,"source_version_id":row.try_get::<Uuid,_>("id")?,"version":row.try_get::<i64,_>("version")?,"content_hash":content_hash,"changed":false}));
                continue;
            }
            let version:i64=sqlx::query_scalar("SELECT coalesce(max(version),0)+1 FROM source_versions WHERE ledger_id=$1 AND source_id=$2").bind(s.ledger_id).bind(source_id).fetch_one(&mut *s.tx).await?;
            let id = Uuid::new_v4();
            sqlx::query("INSERT INTO source_versions(ledger_id,source_id,id,version,body,content_hash,capture_kind,origin_uri,operation_id) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9)").bind(s.ledger_id).bind(source_id).bind(id).bind(version).bind(source.body).bind(&content_hash).bind(capture).bind(source.origin_uri).bind(operation_id).execute(&mut *s.tx).await?;
            sqlx::query("UPDATE sources SET current_version_id=$3 WHERE ledger_id=$1 AND id=$2")
                .bind(s.ledger_id)
                .bind(source_id)
                .bind(id)
                .execute(&mut *s.tx)
                .await?;
            changed = true;
            results.push(json!({"source_id":source_id,"source_version_id":id,"version":version,"content_hash":content_hash,"changed":true}));
        }
        if changed {
            s.bump().await?;
        }
        let response = envelope(json!({"sources":results}), s.revision);
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
    pub async fn sources_get(&self, p: &Principal, request: SourcesGet) -> Result<Value> {
        let mut s = self.session(p, request.ledger_id, false).await?;
        let id = if let Some(id) = request.source_version_id {
            id
        } else {
            sqlx::query_scalar(
                "SELECT current_version_id FROM sources WHERE ledger_id=$1 AND id=$2",
            )
            .bind(s.ledger_id)
            .bind(request.source_id.ok_or_else(|| {
                Error::Invalid("source_id or source_version_id is required".into())
            })?)
            .fetch_optional(&mut *s.tx)
            .await?
            .ok_or(Error::NotFound)?
        };
        let source:Value=sqlx::query_scalar("SELECT to_jsonb(v)||jsonb_build_object('provider',s.provider,'source_scope',s.source_scope,'external_id',s.external_id) FROM source_versions v JOIN sources s ON s.ledger_id=v.ledger_id AND s.id=v.source_id WHERE v.ledger_id=$1 AND v.id=$2").bind(s.ledger_id).bind(id).fetch_optional(&mut *s.tx).await?.ok_or(Error::NotFound)?;
        if let Some(source_id) = request.source_id {
            require(
                source["source_id"] == json!(source_id),
                "Source ID and version do not correspond",
            )?;
        }
        let excerpt = request.record_locator.as_ref().and_then(|locator| {
            source
                .get("body")
                .and_then(Value::as_str)
                .and_then(|body| super::reconcile::source_location(body, locator))
        });
        let positions:Vec<Value>=sqlx::query_scalar("SELECT to_jsonb(p)-'ledger_id' FROM source_positions p WHERE ledger_id=$1 AND source_version_id=$2 AND ($3::text IS NULL OR record_locator=$3) ORDER BY record_locator,kind").bind(s.ledger_id).bind(id).bind(request.record_locator).fetch_all(&mut *s.tx).await?;
        let response = envelope(
            json!({"source":source,"positions":positions,"excerpt":excerpt,"authenticity":"submitter_declared; hash verifies stored content only"}),
            s.revision,
        );
        s.tx.commit().await?;
        Ok(response)
    }
    pub async fn observations_ingest(
        &self,
        p: &Principal,
        request: ObservationsIngest,
    ) -> Result<Value> {
        require(
            request.observations.len() <= 100
                && (!request.observations.is_empty() || request.batch.is_some()),
            "Submit 1..100 observations or a batch update",
        )?;
        let raw = serde_json::to_value(&request)?;
        let fingerprint = hash(&raw)?;
        let tool = "sumi_observations_ingest";
        let operation_id = Uuid::new_v4();
        let mut s = self.session(p, request.ledger_id, true).await?;
        if let Some(v) = s
            .replay(p, tool, &request.idempotency_key, &fingerprint)
            .await?
        {
            return Ok(v);
        }
        let mut results = vec![];
        let mut changed = false;
        let mut touched = std::collections::BTreeSet::new();
        let mut observation_ids = vec![];
        for input in request.observations {
            require(
                !input.reason.trim().is_empty() && input.reason.len() <= 4096,
                "Observation writes need a reason",
            )?;
            let explicit = input.observation_id.is_some();
            let mut id = input.observation_id;
            if id.is_none() {
                let source_version = input.source_version_id.ok_or_else(|| {
                    Error::Invalid("New observation needs source_version_id".into())
                })?;
                let locator = input
                    .record_locator
                    .as_deref()
                    .ok_or_else(|| Error::Invalid("New observation needs record_locator".into()))?;
                let kind = input
                    .payload
                    .as_ref()
                    .ok_or_else(|| Error::Invalid("New observation needs payload".into()))?
                    .kind();
                id=sqlx::query_scalar("SELECT observation_id FROM source_positions WHERE ledger_id=$1 AND source_version_id=$2 AND record_locator=$3 AND kind=$4").bind(s.ledger_id).bind(source_version).bind(locator).bind(kind).fetch_optional(&mut *s.tx).await?;
            }
            let old = if let Some(id) = id {
                Some(s.observation(id).await?)
            } else {
                None
            };
            let id = id.unwrap_or_else(Uuid::new_v4);
            require(
                touched.insert(id),
                "An observation can appear once per ingest batch",
            )?;
            observation_ids.push(id);
            if explicit {
                let old = old.as_ref().ok_or(Error::NotFound)?;
                version(
                    input
                        .expected_version
                        .ok_or_else(|| Error::Invalid("expected_version is required".into()))?,
                    old.version,
                )?;
            }
            let source_version_id = input
                .source_version_id
                .or(old.as_ref().map(|o| o.source_version_id))
                .ok_or_else(|| Error::Invalid("Missing source version".into()))?;
            let (source_id, source_version) = s.source_identity(source_version_id).await?;
            if let Some(old) = &old {
                require(
                    old.source_id == source_id,
                    "An observation cannot move to a different logical source",
                )?;
            }
            if old.is_none() && source_version > 1 && !input.new_record_confirmed {
                let prior: bool = sqlx::query_scalar(
                    "SELECT EXISTS(SELECT 1 FROM observations WHERE ledger_id=$1 AND source_id=$2)",
                )
                .bind(s.ledger_id)
                .bind(source_id)
                .fetch_one(&mut *s.tx)
                .await?;
                if prior {
                    return Err(rule(
                        "SOURCE_CONFLICT",
                        "Material changed; map this row to its stable observation_id, or explicitly confirm a new row",
                        json!({"source_id":source_id,"source_version_id":source_version_id}),
                    ));
                }
            }
            let mut payload = input
                .payload
                .or_else(|| old.as_ref().map(|o| o.payload.clone()))
                .ok_or_else(|| Error::Invalid("Missing payload".into()))?;
            if let (Some(old), ObservationPayload::Statement { statement }) = (&old, &mut payload)
                && let ObservationPayload::Statement {
                    statement: previous,
                } = &old.payload
            {
                if statement.expected_count.is_none() {
                    statement.expected_count = previous.expected_count.clone();
                }
                if statement.expected_debit_total.is_none() {
                    statement.expected_debit_total = previous.expected_debit_total.clone();
                }
                if statement.expected_credit_total.is_none() {
                    statement.expected_credit_total = previous.expected_credit_total.clone();
                }
            }
            if !input.clear_controls.is_empty() {
                require(old.is_some(), "Only an existing control can be corrected")?;
                let locator = input.control_correction_locator.as_ref().ok_or_else(|| {
                    Error::Invalid(
                        "Clearing a mistaken control needs a source correction locator".into(),
                    )
                })?;
                let body: String = sqlx::query_scalar(
                    "SELECT body FROM source_versions WHERE ledger_id=$1 AND id=$2",
                )
                .bind(s.ledger_id)
                .bind(source_version_id)
                .fetch_one(&mut *s.tx)
                .await?;
                require(
                    super::reconcile::source_location(&body, locator).is_some(),
                    "Control correction locator must resolve in the archived source",
                )?;
                if let ObservationPayload::Statement { statement } = &mut payload {
                    for field in &input.clear_controls {
                        match field.as_str() {
                            "count" => statement.expected_count = None,
                            "debit_total" => statement.expected_debit_total = None,
                            "credit_total" => statement.expected_credit_total = None,
                            _ => return Err(Error::Invalid("Unknown control name".into())),
                        }
                    }
                } else {
                    return Err(Error::Invalid(
                        "Only statement controls can be cleared".into(),
                    ));
                }
            }
            let record_locator = input
                .record_locator
                .or_else(|| old.as_ref().map(|o| o.record_locator.clone()))
                .ok_or_else(|| Error::Invalid("Missing record locator".into()))?;
            require(
                !record_locator.is_empty() && record_locator.len() <= 1024,
                "Invalid record locator",
            )?;
            s.validate_observation(&payload).await?;
            let references = input
                .references
                .or_else(|| old.as_ref().map(|o| o.references.clone()))
                .unwrap_or_default();
            require(
                references.len() <= 100
                    && references.iter().all(|r| !r.is_empty() && r.len() <= 4096),
                "Invalid source references",
            )?;
            let extractor = input
                .extractor
                .or_else(|| old.as_ref().map(|o| o.extractor.clone()))
                .unwrap_or_else(|| p.client_id.clone());
            require(
                !extractor.is_empty() && extractor.len() <= 512,
                "Invalid extractor",
            )?;
            let payload_changed = old.as_ref().is_none_or(|o| {
                o.source_version_id != source_version_id
                    || o.record_locator != record_locator
                    || o.payload != payload
                    || o.references != references
                    || o.extractor != extractor
            });
            let review_note = if input.clear_review_note {
                None
            } else {
                input
                    .review_note
                    .or_else(|| old.as_ref().and_then(|o| o.review_note.clone()))
            };
            let ignored_reason = if (payload_changed && old.is_some()) || input.clear_ignored_reason
            {
                None
            } else {
                input
                    .ignored_reason
                    .or_else(|| old.as_ref().and_then(|o| o.ignored_reason.clone()))
            };
            require(
                review_note.as_ref().is_none_or(|s| s.len() <= 8192)
                    && ignored_reason
                        .as_ref()
                        .is_none_or(|s| !s.trim().is_empty() && s.len() <= 4096),
                "Invalid note or ignore reason",
            )?;
            require(
                ignored_reason.is_none() || !matches!(payload, ObservationPayload::Movement { .. }),
                "Real movement rows cannot be ignored",
            )?;
            let position:Option<Uuid>=sqlx::query_scalar("SELECT observation_id FROM source_positions WHERE ledger_id=$1 AND source_version_id=$2 AND record_locator=$3 AND kind=$4").bind(s.ledger_id).bind(source_version_id).bind(&record_locator).bind(payload.kind()).fetch_optional(&mut *s.tx).await?;
            if position.is_some_and(|existing| existing != id) {
                return Err(rule(
                    "SOURCE_CONFLICT",
                    "This source position already belongs to another stable observation",
                    json!({"observation_id":position,"source_version_id":source_version_id}),
                ));
            }
            let mut observed = Observed {
                id,
                source_id,
                kind: payload.kind().into(),
                version: old.as_ref().map_or(1, |o| o.version),
                current_version: old.as_ref().map_or(1, |o| o.current_version),
                source_version_id,
                record_locator,
                payload,
                references,
                extractor,
                review_note,
                ignored_reason,
            };
            let entity_changed = old.as_ref() != Some(&observed);
            if !explicit && old.is_some() && entity_changed {
                return Err(rule(
                    "VERSION_CONFLICT",
                    "Existing source row changed; supply its stable observation_id and expected_version",
                    json!({"observation_id":id,"version":observed.version}),
                ));
            }
            if entity_changed {
                if let Some(old) = &old {
                    observed.version = old.version.checked_add(1).ok_or(Error::Overflow)?;
                    if payload_changed {
                        observed.current_version =
                            old.current_version.checked_add(1).ok_or(Error::Overflow)?;
                    }
                }
                sqlx::query("INSERT INTO observations(ledger_id,source_id,id,kind,current_version,version,review_note,ignored_reason) VALUES($1,$2,$3,$4,$5,$6,$7,$8) ON CONFLICT(ledger_id,id) DO UPDATE SET kind=EXCLUDED.kind,current_version=EXCLUDED.current_version,version=EXCLUDED.version,review_note=EXCLUDED.review_note,ignored_reason=EXCLUDED.ignored_reason")
                    .bind(s.ledger_id).bind(source_id).bind(id).bind(&observed.kind).bind(observed.current_version).bind(observed.version).bind(&observed.review_note).bind(&observed.ignored_reason).execute(&mut *s.tx).await?;
                if payload_changed {
                    sqlx::query("INSERT INTO observation_versions(ledger_id,observation_id,version,source_version_id,record_locator,payload,references_json,extractor,reason,operation_id) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)")
                        .bind(s.ledger_id).bind(id).bind(observed.current_version).bind(source_version_id).bind(&observed.record_locator).bind(serde_json::to_value(&observed.payload)?).bind(json!(observed.references)).bind(&observed.extractor).bind(&input.reason).bind(operation_id).execute(&mut *s.tx).await?;
                }
                sqlx::query("INSERT INTO observation_history(ledger_id,observation_id,version,snapshot,operation_id) VALUES($1,$2,$3,$4,$5)").bind(s.ledger_id).bind(id).bind(observed.version).bind(json!({"observation":observed,"reason":input.reason})).bind(operation_id).execute(&mut *s.tx).await?;
                sqlx::query("INSERT INTO source_positions(ledger_id,source_version_id,record_locator,kind,observation_id) VALUES($1,$2,$3,$4,$5) ON CONFLICT DO NOTHING").bind(s.ledger_id).bind(source_version_id).bind(&observed.record_locator).bind(&observed.kind).bind(id).execute(&mut *s.tx).await?;
                changed = true;
            }
            let status = s.observation_status(&observed).await?;
            results.push(json!({"observation":observed,"changed":entity_changed,"status":status}));
        }
        let batch = if let Some(input) = request.batch {
            let (value, batch_changed) = s.upsert_batch(input, &observation_ids).await?;
            changed |= batch_changed;
            Some(value)
        } else {
            None
        };
        if changed {
            s.bump().await?;
        }
        let response = envelope(json!({"observations":results,"batch":batch}), s.revision);
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
    pub async fn observations_search(
        &self,
        p: &Principal,
        request: ObservationsSearch,
    ) -> Result<Value> {
        let mut s = self.session(p, request.ledger_id, false).await?;
        let filter = hash(&(
            request.ledger_id,
            request.observation_id,
            request.include_history,
            request.source_id,
            request.source_version_id,
            request.account_id,
            &request.observation_type,
            request.pending_only,
        ))?;
        let mut values = vec![];
        for o in s.observations().await? {
            if request.observation_id.is_some_and(|id| o.id != id)
                || request.source_id.is_some_and(|id| o.source_id != id)
                || request
                    .source_version_id
                    .is_some_and(|id| o.source_version_id != id)
                || request
                    .observation_type
                    .as_ref()
                    .is_some_and(|t| t != &o.kind)
            {
                continue;
            }
            if let Some(id) = request.account_id
                && observation_account(&o.payload) != Some(id)
            {
                continue;
            }
            let status = s.observation_status(&o).await?;
            if request.pending_only && !status["pending"].as_bool().unwrap_or(false) {
                continue;
            }
            let history: Option<Vec<Value>> = if request.include_history {
                Some(sqlx::query_scalar("SELECT to_jsonb(h)-'ledger_id' FROM observation_history h WHERE ledger_id=$1 AND observation_id=$2 ORDER BY version").bind(s.ledger_id).bind(o.id).fetch_all(&mut *s.tx).await?)
            } else {
                None
            };
            values.push(json!({"observation":o,"status":status,"history":history}));
        }
        let batches = s.batch_summaries().await?;
        let data = page(
            values,
            request.cursor.as_deref(),
            request.limit,
            s.revision,
            &filter,
        )?;
        let response = envelope(
            json!({"items":data["items"],"next_cursor":data["next_cursor"],"import_batches":batches}),
            s.revision,
        );
        s.tx.commit().await?;
        Ok(response)
    }
    pub async fn fx_rates_ingest(&self, p: &Principal, request: FxRatesIngest) -> Result<Value> {
        require(
            !request.rates.is_empty() && request.rates.len() <= 100,
            "Submit 1..100 quotes",
        )?;
        let raw = serde_json::to_value(&request)?;
        let fingerprint = hash(&raw)?;
        let tool = "sumi_fx_rates_ingest";
        let operation_id = Uuid::new_v4();
        let mut s = self.session(p, request.ledger_id, true).await?;
        if let Some(v) = s
            .replay(p, tool, &request.idempotency_key, &fingerprint)
            .await?
        {
            return Ok(v);
        }
        let mut results = vec![];
        let mut changed = false;
        for rate in request.rates {
            currency_precision(&rate.from_currency)?;
            currency_precision(&rate.to_currency)?;
            parse_rate(&rate.rate)?;
            require(
                rate.from_currency != rate.to_currency,
                "A quote must connect different currencies",
            )?;
            require(
                !rate.record_locator.is_empty() && rate.record_locator.len() <= 1024,
                "Quote requires a source locator",
            )?;
            s.require_source_version(rate.source_version_id).await?;
            let id = Uuid::new_v5(&s.ledger_id, serde_json::to_string(&rate)?.as_bytes());
            let inserted=sqlx::query("INSERT INTO fx_rates(ledger_id,id,from_currency,to_currency,on_date,rate,source_version_id,record_locator,recorded_revision,operation_id) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10) ON CONFLICT DO NOTHING")
                .bind(s.ledger_id).bind(id).bind(&rate.from_currency).bind(&rate.to_currency).bind(rate.on).bind(&rate.rate).bind(rate.source_version_id).bind(&rate.record_locator).bind(s.revision.checked_add(1).ok_or(Error::Overflow)?).bind(operation_id).execute(&mut *s.tx).await?.rows_affected()>0;
            changed |= inserted;
            results.push(json!({"rate_id":id,"rate":rate,"changed":inserted}));
        }
        if changed {
            s.bump().await?;
        }
        let response = envelope(json!({"rates":results}), s.revision);
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
    pub async fn fx_rates_query(&self, p: &Principal, request: FxRatesQuery) -> Result<Value> {
        let mut s = self.session(p, request.ledger_id, false).await?;
        let filter = hash(&(&request.from_currency, &request.to_currency, request.on))?;
        let rates:Vec<Value>=sqlx::query_scalar("SELECT to_jsonb(r)-'ledger_id' FROM fx_rates r WHERE ledger_id=$1 AND ($2::text IS NULL OR from_currency=$2) AND ($3::text IS NULL OR to_currency=$3) AND ($4::date IS NULL OR on_date=$4) ORDER BY on_date,id").bind(s.ledger_id).bind(request.from_currency).bind(request.to_currency).bind(request.on).fetch_all(&mut *s.tx).await?;
        let response = envelope(
            page(
                rates,
                request.cursor.as_deref(),
                request.limit,
                s.revision,
                &filter,
            )?,
            s.revision,
        );
        s.tx.commit().await?;
        Ok(response)
    }
}
pub(super) fn observation_account(payload: &ObservationPayload) -> Option<Uuid> {
    match payload {
        ObservationPayload::Movement { account_id, .. } => *account_id,
        ObservationPayload::Balance { account_id, .. } => Some(*account_id),
        ObservationPayload::Statement { statement } => Some(statement.account_id),
        _ => None,
    }
}
impl Session {
    pub(super) async fn require_source_version(&mut self, id: Uuid) -> Result<()> {
        self.source_identity(id).await?;
        Ok(())
    }
    pub(super) async fn source_identity(&mut self, id: Uuid) -> Result<(Uuid, i64)> {
        let row = sqlx::query(
            "SELECT source_id,version FROM source_versions WHERE ledger_id=$1 AND id=$2",
        )
        .bind(self.ledger_id)
        .bind(id)
        .fetch_optional(&mut *self.tx)
        .await?
        .ok_or(Error::NotFound)?;
        Ok((row.try_get("source_id")?, row.try_get("version")?))
    }
    pub(super) async fn observation(&mut self, id: Uuid) -> Result<Observed> {
        let row=sqlx::query("SELECT o.*,v.source_version_id,v.record_locator,v.payload,v.references_json,v.extractor FROM observations o JOIN observation_versions v ON v.ledger_id=o.ledger_id AND v.observation_id=o.id AND v.version=o.current_version WHERE o.ledger_id=$1 AND o.id=$2").bind(self.ledger_id).bind(id).fetch_optional(&mut *self.tx).await?.ok_or(Error::NotFound)?;
        observed_row(row)
    }
    pub(super) async fn observations(&mut self) -> Result<Vec<Observed>> {
        let rows=sqlx::query("SELECT o.*,v.source_version_id,v.record_locator,v.payload,v.references_json,v.extractor FROM observations o JOIN observation_versions v ON v.ledger_id=o.ledger_id AND v.observation_id=o.id AND v.version=o.current_version WHERE o.ledger_id=$1 ORDER BY o.id").bind(self.ledger_id).fetch_all(&mut *self.tx).await?;
        rows.into_iter().map(observed_row).collect()
    }
    pub(super) async fn validate_observation(
        &mut self,
        payload: &ObservationPayload,
    ) -> Result<()> {
        let accounts = self.accounts().await?;
        if let Some(id) = observation_account(payload) {
            let a = accounts.get(&id).ok_or(Error::NotFound)?;
            require(
                a.role.is_none(),
                "Observation must identify a real financial account",
            )?;
        }
        match payload {
            ObservationPayload::Movement {
                account_id, money, ..
            } => {
                if let Some(m) = money {
                    m.validate()?;
                    m.amount_minor.positive()?;
                    if let Some(id) = account_id {
                        require(
                            accounts.get(id).is_some_and(|a| a.currency == m.currency),
                            "Movement account and currency differ",
                        )?;
                    }
                }
            }
            ObservationPayload::Order { money, .. }
            | ObservationPayload::Authorization { money, .. } => {
                if let Some(m) = money {
                    m.validate()?;
                    m.amount_minor.positive()?;
                }
            }
            ObservationPayload::Balance {
                account_id,
                currency,
                ..
            } => {
                currency_precision(currency)?;
                require(
                    accounts
                        .get(account_id)
                        .is_some_and(|a| &a.currency == currency),
                    "Balance currency differs from account",
                )?;
            }
            ObservationPayload::FxQuote {
                from_currency,
                to_currency,
                rate,
                ..
            } => {
                currency_precision(from_currency)?;
                currency_precision(to_currency)?;
                parse_rate(rate)?;
            }
            ObservationPayload::Statement { statement: s } => {
                currency_precision(&s.currency)?;
                require(
                    accounts
                        .get(&s.account_id)
                        .is_some_and(|a| a.currency == s.currency),
                    "Statement currency differs from account",
                )?;
                require(
                    s.period_start < s.period_end,
                    "Statement period must be [start,end)",
                )?;
                if let Some(c) = &s.expected_count {
                    require(
                        !c.locator.is_empty(),
                        "Control count requires source locator",
                    )?;
                }
                for c in [&s.expected_debit_total, &s.expected_credit_total]
                    .into_iter()
                    .flatten()
                {
                    require(
                        c.value >= Amount::ZERO && !c.locator.is_empty(),
                        "Controls require nonnegative totals and locators",
                    )?;
                }
                if let Some(locators) = &s.record_locators {
                    require(
                        locators.len() <= 100000
                            && locators.iter().all(|s| !s.is_empty() && s.len() <= 1024),
                        "Invalid record locator range",
                    )?;
                    require(
                        locators
                            .iter()
                            .collect::<std::collections::BTreeSet<_>>()
                            .len()
                            == locators.len(),
                        "Record range contains duplicate locators",
                    )?;
                }
            }
        }
        Ok(())
    }
    pub(super) async fn upsert_batch(
        &mut self,
        input: BatchInput,
        observations: &[Uuid],
    ) -> Result<(Value, bool)> {
        require(
            !input.name.is_empty()
                && input.name.len() <= 256
                && input.source_version_ids.len() <= 100,
            "Invalid import batch",
        )?;
        let id = input.id.unwrap_or_else(Uuid::new_v4);
        let old = sqlx::query(
            "SELECT version,name,complete FROM import_batches WHERE ledger_id=$1 AND id=$2",
        )
        .bind(self.ledger_id)
        .bind(id)
        .fetch_optional(&mut *self.tx)
        .await?;
        let old_version = if let Some(row) = &old {
            let v = row.try_get::<i64, _>("version")?;
            version(
                input
                    .expected_version
                    .ok_or_else(|| Error::Invalid("Batch expected_version is required".into()))?,
                v,
            )?;
            v
        } else {
            require(
                input.id.is_none() && input.expected_version.is_none(),
                "Import batch not found",
            )?;
            0
        };
        let metadata_changed = old.as_ref().is_none_or(|r| {
            r.get::<String, _>("name") != input.name
                || r.get::<bool, _>("complete") != input.complete
        });
        let mut changed = metadata_changed;
        if old.is_none() {
            sqlx::query("INSERT INTO import_batches(ledger_id,id,version,name,complete) VALUES($1,$2,1,$3,$4)").bind(self.ledger_id).bind(id).bind(&input.name).bind(input.complete).execute(&mut *self.tx).await?;
        }
        for source in input.source_version_ids {
            self.require_source_version(source).await?;
            changed|=sqlx::query("INSERT INTO batch_sources(ledger_id,batch_id,source_version_id) VALUES($1,$2,$3) ON CONFLICT DO NOTHING").bind(self.ledger_id).bind(id).bind(source).execute(&mut *self.tx).await?.rows_affected()>0;
        }
        for observation in observations {
            changed|=sqlx::query("INSERT INTO batch_observations(ledger_id,batch_id,observation_id) VALUES($1,$2,$3) ON CONFLICT DO NOTHING").bind(self.ledger_id).bind(id).bind(observation).execute(&mut *self.tx).await?.rows_affected()>0;
        }
        if changed && old.is_some() {
            sqlx::query("UPDATE import_batches SET version=$3,name=$4,complete=$5 WHERE ledger_id=$1 AND id=$2").bind(self.ledger_id).bind(id).bind(old_version.checked_add(1).ok_or(Error::Overflow)?).bind(input.name).bind(input.complete).execute(&mut *self.tx).await?;
        }
        let result = self.batch_summary(id).await?;
        Ok((result, changed))
    }
    pub(super) async fn batch_summary(&mut self, id: Uuid) -> Result<Value> {
        let batch: Value = sqlx::query_scalar(
            "SELECT to_jsonb(b)-'ledger_id' FROM import_batches b WHERE ledger_id=$1 AND id=$2",
        )
        .bind(self.ledger_id)
        .bind(id)
        .fetch_optional(&mut *self.tx)
        .await?
        .ok_or(Error::NotFound)?;
        let ids:Vec<Uuid>=sqlx::query_scalar("SELECT observation_id FROM batch_observations WHERE ledger_id=$1 AND batch_id=$2 ORDER BY observation_id").bind(self.ledger_id).bind(id).fetch_all(&mut *self.tx).await?;
        let mut count = 0;
        let mut incomplete = vec![];
        let mut totals = BTreeMap::<String, (Amount, Amount)>::new();
        for id in ids {
            let o = self.observation(id).await?;
            if let ObservationPayload::Movement {
                money, direction, ..
            } = o.payload
            {
                count += 1;
                if let (Some(m), Some(d)) = (money, direction) {
                    let t = totals.entry(m.currency).or_default();
                    if d == Direction::In {
                        t.1 = t.1.checked_add(m.amount_minor)?;
                    } else {
                        t.0 = t.0.checked_add(m.amount_minor)?;
                    }
                } else {
                    incomplete.push(id);
                }
            }
        }
        Ok(
            json!({"batch":batch,"actual_count":count,"incomplete_movement_ids":incomplete,"actual_totals":totals.into_iter().map(|(currency,(debit,credit))|json!({"currency":currency,"debit_minor":debit,"credit_minor":credit})).collect::<Vec<_>>() }),
        )
    }
    pub(super) async fn batch_summaries(&mut self) -> Result<Vec<Value>> {
        let ids: Vec<Uuid> = sqlx::query_scalar(
            "SELECT id FROM import_batches WHERE ledger_id=$1 ORDER BY id LIMIT 100",
        )
        .bind(self.ledger_id)
        .fetch_all(&mut *self.tx)
        .await?;
        let mut results = vec![];
        for id in ids {
            results.push(self.batch_summary(id).await?);
        }
        Ok(results)
    }
}
fn observed_row(row: sqlx::postgres::PgRow) -> Result<Observed> {
    Ok(Observed {
        id: row.try_get("id")?,
        source_id: row.try_get("source_id")?,
        kind: row.try_get("kind")?,
        version: row.try_get("version")?,
        current_version: row.try_get("current_version")?,
        source_version_id: row.try_get("source_version_id")?,
        record_locator: row.try_get("record_locator")?,
        payload: serde_json::from_value(row.try_get("payload")?)?,
        references: serde_json::from_value(row.try_get("references_json")?)?,
        extractor: row.try_get("extractor")?,
        review_note: row.try_get("review_note")?,
        ignored_reason: row.try_get("ignored_reason")?,
    })
}
