use super::*;

pub(super) const EXPORT_TABLES: &[&str] = &[
    "users",
    "ledgers",
    "authorization_grants",
    "accounts",
    "preferences",
    "categories",
    "tags",
    "sources",
    "source_versions",
    "observations",
    "observation_versions",
    "observation_history",
    "source_positions",
    "import_batches",
    "batch_sources",
    "batch_observations",
    "operations",
    "transactions",
    "transaction_versions",
    "journal_entries",
    "journal_lines",
    "evidence_links",
    "transaction_identifiers",
    "fx_rates",
    "reconciliations",
];
impl Session {
    pub(super) async fn export_snapshot(&mut self, p: &Principal) -> Result<Value> {
        let mut tables = serde_json::Map::new();
        for table in EXPORT_TABLES {
            let (filter, binding) = match *table {
                "users" => ("id", p.user_id()),
                "ledgers" => ("id", self.ledger_id),
                _ => ("ledger_id", self.ledger_id),
            };
            let expression = match *table {
                "journal_lines" | "evidence_links" => {
                    "(to_jsonb(t)||jsonb_build_object('amount_minor',t.amount_minor::text))"
                }
                "journal_entries" => "(to_jsonb(t)-'created_xid'-'created_tx_at')",
                _ => "to_jsonb(t)",
            };
            let rows: Vec<Value> = sqlx::query_scalar(&format!(
                "SELECT {expression} FROM {table} t WHERE {filter}=$1 ORDER BY {expression}::text"
            ))
            .bind(binding)
            .fetch_all(&mut *self.tx)
            .await?;
            tables.insert((*table).into(), json!(rows));
        }
        let fingerprint = hash(&tables)?;
        Ok(
            json!({"format":"sumi-export-v1","ledger_id":self.ledger_id,"ledger_revision":self.revision,"tables":tables,"content_hash":fingerprint,"snapshot":"single PostgreSQL repeatable-read transaction"}),
        )
    }
}

/// Administrative recovery into an empty ledger namespace. Never exposed as an MCP tool.
pub async fn restore_export(pool: &PgPool, document: Value) -> Result<Uuid> {
    require(
        document["format"] == "sumi-export-v1",
        "Unsupported export format",
    )?;
    let tables = document["tables"]
        .as_object()
        .ok_or_else(|| Error::Invalid("Export has no tables".into()))?;
    require(
        document["content_hash"] == hash(tables)?,
        "Export content hash mismatch",
    )?;
    require(
        tables.len() == EXPORT_TABLES.len()
            && EXPORT_TABLES.iter().all(|t| tables.contains_key(*t)),
        "Export table set differs from this schema version",
    )?;
    let user_rows = tables["users"].as_array().ok_or(Error::NotFound)?;
    let ledger_rows = tables["ledgers"].as_array().ok_or(Error::NotFound)?;
    require(
        user_rows.len() == 1 && ledger_rows.len() == 1,
        "A personal export contains exactly one user and ledger",
    )?;
    let ledger_id: Uuid = serde_json::from_value(document["ledger_id"].clone())?;
    let user_id: Uuid = serde_json::from_value(user_rows[0]["id"].clone())?;
    require(
        ledger_rows[0]["id"] == json!(ledger_id)
            && ledger_rows[0]["owner_user_id"] == json!(user_id),
        "Export ownership is inconsistent",
    )?;
    let allowed:bool=sqlx::query_scalar("SELECT rolsuper OR pg_has_role(current_user,(SELECT relowner FROM pg_class WHERE oid='ledgers'::regclass),'MEMBER') FROM pg_roles WHERE rolname=current_user").fetch_one(pool).await?;
    require(
        allowed,
        "Recovery requires the migration owner or database administrator",
    )?;
    let mut tx = pool.begin().await?;
    sqlx::query("SELECT set_config('sumi.user_id',$1,true)")
        .bind(user_id.to_string())
        .execute(&mut *tx)
        .await?;
    let existing:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM users WHERE id=$1) OR EXISTS(SELECT 1 FROM ledgers WHERE id=$2)").bind(user_id).bind(ledger_id).fetch_one(&mut *tx).await?;
    require(
        !existing,
        "Recovery target already has this user or ledger; no records were replaced",
    )?;
    sqlx::query("SET CONSTRAINTS ALL DEFERRED")
        .execute(&mut *tx)
        .await?;
    for table in EXPORT_TABLES {
        let rows = tables[*table]
            .as_array()
            .ok_or_else(|| Error::Invalid(format!("Invalid rows for {table}")))?;
        for row in rows {
            let expected = match *table {
                "users" => row["id"] == json!(user_id),
                "ledgers" => row["id"] == json!(ledger_id),
                _ => row["ledger_id"] == json!(ledger_id),
            };
            require(expected, "Export contains rows from a different ledger")?;
            if *table == "source_versions" {
                let body = row["body"].as_str().ok_or(Error::NotFound)?;
                require(
                    row["content_hash"] == format!("{:x}", Sha256::digest(body.as_bytes())),
                    "Archived source content hash mismatch",
                )?;
            }
        }
        let columns:Vec<String>=sqlx::query_scalar("SELECT column_name::text FROM information_schema.columns WHERE table_schema='public' AND table_name=$1 AND column_name NOT IN ('created_xid','created_tx_at') ORDER BY ordinal_position").bind(table).fetch_all(&mut *tx).await?;
        require(!columns.is_empty(), "Recovery schema is missing a table")?;
        let columns = columns
            .iter()
            .map(|c| format!("\"{c}\""))
            .collect::<Vec<_>>()
            .join(",");
        sqlx::query(&format!("INSERT INTO {table} ({columns}) SELECT {columns} FROM jsonb_populate_recordset(NULL::{table},$1::jsonb)")).bind(json!(rows)).execute(&mut *tx).await?;
    }
    let revision: i64 = serde_json::from_value(ledger_rows[0]["revision"].clone())?;
    let mut session = Session {
        tx,
        ledger_id,
        revision,
    };
    session.validate_refunds().await?;
    session.validate_evidence(Uuid::nil()).await?;
    session.tx.commit().await?;
    Ok(ledger_id)
}
