use crate::{
    api::*,
    auth::Principal,
    domain::*,
    error::{Error, Result, require, version},
};
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Postgres, Row, Transaction};
use std::collections::BTreeMap;
use uuid::Uuid;
mod evidence;
mod export;
mod imports;
mod preferences;
mod quality;
mod reconcile;
mod reports;
mod transactions;
pub use export::restore_export;

#[derive(Clone)]
pub struct Store {
    pub pool: PgPool,
}

struct Session {
    tx: Transaction<'static, Postgres>,
    ledger_id: Uuid,
    revision: i64,
}

struct WriteOperation<'a> {
    principal: &'a Principal,
    tool: &'a str,
    key: &'a str,
    request_hash: &'a str,
    request: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CurrentTransaction {
    id: Uuid,
    version: i64,
    state: String,
    transaction: TransactionData,
}

fn hash(value: &impl Serialize) -> Result<String> {
    Ok(format!("{:x}", Sha256::digest(serde_json::to_vec(value)?)))
}

fn envelope(data: Value, revision: i64) -> Value {
    json!({"data":data,"ledger_revision":revision,"warnings":[],"next_actions":[]})
}

fn account_row(row: sqlx::postgres::PgRow) -> Result<Account> {
    Ok(Account {
        id: row.try_get("id")?,
        name: row.try_get("name")?,
        kind: serde_json::from_value(json!(row.try_get::<String, _>("kind")?))?,
        currency: row.try_get("currency")?,
        role: row.try_get("role")?,
        active: row.try_get("active")?,
        version: row.try_get("version")?,
        parent_id: row.try_get("parent_id")?,
        external_identifiers: serde_json::from_value(row.try_get("external_identifiers")?)?,
    })
}

impl Store {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Reject superuser, BYPASSRLS, or table ownership before serving requests.
    pub async fn verify_runtime_role(&self) -> Result<()> {
        let unsafe_role: bool = sqlx::query_scalar("SELECT rolsuper OR rolbypassrls OR EXISTS (SELECT 1 FROM pg_class WHERE relname=ANY($1) AND relnamespace='public'::regnamespace AND (pg_has_role(current_user,relowner,'MEMBER') OR NOT relrowsecurity OR NOT relforcerowsecurity)) FROM pg_roles WHERE rolname=current_user")
            .bind(export::EXPORT_TABLES).fetch_one(&self.pool).await?;
        let table_count:i64=sqlx::query_scalar("SELECT count(*) FROM pg_class WHERE relname=ANY($1) AND relnamespace='public'::regnamespace").bind(export::EXPORT_TABLES).fetch_one(&self.pool).await?;
        require(
            table_count == export::EXPORT_TABLES.len() as i64,
            "Database migrations are incomplete",
        )?;
        require(
            !unsafe_role,
            "HTTP runtime must use a separate non-owner role without SUPERUSER or BYPASSRLS and all tables must enforce RLS",
        )?;
        sqlx::query("SELECT revision FROM ledgers LIMIT 0")
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn identity_transaction(
        &self,
        principal: &Principal,
    ) -> Result<Transaction<'static, Postgres>> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("SELECT set_config('sumi.user_id',$1,true),set_config('lock_timeout','5s',true),set_config('statement_timeout','15s',true)")
            .bind(principal.user_id().to_string()).execute(&mut *tx).await?;
        Ok(tx)
    }

    pub async fn context(&self, principal: &Principal) -> Result<Value> {
        principal.require("sumi:read")?;
        let mut tx = self.identity_transaction(principal).await?;
        let user_id = principal.user_id();
        let ledger_id = Uuid::new_v5(&user_id, b"personal-ledger");
        sqlx::query(
            "INSERT INTO users(id,issuer,subject) VALUES($1,$2,$3) ON CONFLICT(id) DO NOTHING",
        )
        .bind(user_id)
        .bind(&principal.issuer)
        .bind(&principal.subject)
        .execute(&mut *tx)
        .await?;
        sqlx::query("INSERT INTO ledgers(id,owner_user_id) VALUES($1,$2) ON CONFLICT(owner_user_id) DO NOTHING")
            .bind(ledger_id).bind(user_id).execute(&mut *tx).await?;
        let active: bool = sqlx::query_scalar("SELECT status='active' FROM users WHERE id=$1")
            .bind(user_id)
            .fetch_one(&mut *tx)
            .await?;
        if !active {
            return Err(Error::Forbidden);
        }
        sqlx::query("INSERT INTO authorization_grants(ledger_id,user_id,client_id) VALUES($1,$2,$3) ON CONFLICT DO NOTHING").bind(ledger_id).bind(user_id).bind(&principal.client_id).execute(&mut *tx).await?;
        tx.commit().await?;
        let mut session = self.session(principal, ledger_id, false).await?;
        let response = session.context_snapshot().await?;
        session.tx.commit().await?;
        Ok(response)
    }

    async fn session(
        &self,
        principal: &Principal,
        ledger_id: Uuid,
        write: bool,
    ) -> Result<Session> {
        self.session_mode(
            principal,
            ledger_id,
            write,
            if write { "sumi:write" } else { "sumi:read" },
        )
        .await
    }
    async fn session_mode(
        &self,
        principal: &Principal,
        ledger_id: Uuid,
        write: bool,
        scope: &str,
    ) -> Result<Session> {
        principal.require(scope)?;
        let mut tx = self.pool.begin().await?;
        if !write {
            sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
                .execute(&mut *tx)
                .await?;
        }
        sqlx::query("SELECT set_config('sumi.user_id',$1,true),set_config('lock_timeout','5s',true),set_config('statement_timeout','15s',true)")
            .bind(principal.user_id().to_string()).execute(&mut *tx).await?;
        let sql = if write {
            "SELECT revision FROM ledgers WHERE id=$1 AND owner_user_id=$2 FOR UPDATE"
        } else {
            "SELECT revision FROM ledgers WHERE id=$1 AND owner_user_id=$2"
        };
        let revision = sqlx::query_scalar(sql)
            .bind(ledger_id)
            .bind(principal.user_id())
            .fetch_optional(&mut *tx)
            .await?
            .ok_or(Error::NotFound)?;
        let granted:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM authorization_grants g JOIN users u ON u.id=g.user_id WHERE g.ledger_id=$1 AND g.user_id=$2 AND g.client_id=$3 AND g.status='active' AND u.status='active' AND $4=ANY(g.scopes))")
            .bind(ledger_id).bind(principal.user_id()).bind(&principal.client_id).bind(scope).fetch_one(&mut *tx).await?;
        if !granted {
            return Err(Error::Forbidden);
        }
        Ok(Session {
            tx,
            ledger_id,
            revision,
        })
    }

    pub async fn accounts_list(
        &self,
        principal: &Principal,
        request: AccountsList,
    ) -> Result<Value> {
        let mut session = self.session(principal, request.ledger_id, false).await?;
        let accounts = session.accounts().await?;
        let balances = session.balances(request.as_of).await?;
        let openings = session.openings().await?;
        let values = accounts.values().filter(|a| request.include_system || a.role.is_none()).map(|a| {
            json!({"account":a,"balance_minor":balances.get(&a.id).copied().unwrap_or_default(),
                "balance_sign":"debit_positive","opening":opening_status(openings.get(&a.id),request.as_of)})
        }).collect::<Vec<_>>();
        let filter_hash = hash(&(request.as_of, request.include_system))?;
        let data = page(
            values,
            request.cursor.as_deref(),
            request.limit,
            session.revision,
            &filter_hash,
        )?;
        let response = envelope(data, session.revision);
        session.tx.commit().await?;
        Ok(response)
    }

    pub async fn accounts_upsert(
        &self,
        principal: &Principal,
        request: AccountsUpsert,
    ) -> Result<Value> {
        let request_value = serde_json::to_value(&request)?;
        let mut session = self.session(principal, request.ledger_id, true).await?;
        let request_hash = hash(&request_value)?;
        if let Some(result) = session
            .replay(
                principal,
                "sumi_accounts_upsert",
                &request.idempotency_key,
                &request_hash,
            )
            .await?
        {
            return Ok(result);
        }
        require(
            !request.name.trim().is_empty() && request.name.len() <= 256,
            "Account name must contain 1..256 bytes",
        )?;
        require(
            matches!(request.kind, AccountKind::Asset | AccountKind::Liability),
            "Only real asset and liability accounts can be created",
        )?;
        currency_precision(&request.currency)?;
        let accounts = session.accounts().await?;
        let previous = match request.account_id {
            Some(id) => {
                let old = accounts.get(&id).ok_or(Error::NotFound)?;
                require(old.role.is_none(), "System accounts cannot be edited")?;
                version(
                    request
                        .expected_version
                        .ok_or_else(|| Error::Invalid("expected_version is required".into()))?,
                    old.version,
                )?;
                Some(old.clone())
            }
            None => {
                require(
                    request.expected_version.is_none(),
                    "New account cannot have expected_version",
                )?;
                None
            }
        };
        let mut account = Account {
            id: request.account_id.unwrap_or_else(Uuid::new_v4),
            name: request.name,
            kind: request.kind,
            currency: request.currency,
            role: None,
            active: request.active,
            version: previous.as_ref().map_or(1, |a| a.version),
            parent_id: request.parent_id,
            external_identifiers: request.external_identifiers,
        };
        require(
            account.external_identifiers.len() <= 100
                && account.external_identifiers.iter().all(|(a, b)| {
                    !a.is_empty() && a.len() <= 256 && !b.is_empty() && b.len() <= 1024
                }),
            "Invalid external account identifiers",
        )?;
        let mut parents = std::collections::BTreeSet::from([account.id]);
        let mut parent = account.parent_id;
        while let Some(id) = parent {
            require(parents.insert(id), "Account hierarchy contains a cycle")?;
            let ancestor = accounts.get(&id).ok_or(Error::NotFound)?;
            require(
                ancestor.role.is_none(),
                "System accounts cannot be account parents",
            )?;
            parent = ancestor.parent_id;
        }
        let changed = previous.as_ref() != Some(&account);
        if changed {
            if let Some(old) = &previous {
                if old.currency != account.currency || old.kind != account.kind {
                    let used: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM journal_lines WHERE ledger_id=$1 AND account_id=$2) OR EXISTS(SELECT 1 FROM transactions WHERE ledger_id=$1 AND opening_account_id=$2)")
                        .bind(session.ledger_id).bind(account.id).fetch_one(&mut *session.tx).await?;
                    require(
                        !used,
                        "Account with journal history or opening baseline cannot change currency or type",
                    )?;
                }
                account.version = account.version.checked_add(1).ok_or(Error::Overflow)?;
                sqlx::query("UPDATE accounts SET name=$3,kind=$4,currency=$5,active=$6,version=$7,parent_id=$8,external_identifiers=$9 WHERE ledger_id=$1 AND id=$2")
                    .bind(session.ledger_id).bind(account.id).bind(&account.name).bind(kind_text(account.kind)).bind(&account.currency).bind(account.active).bind(account.version).bind(account.parent_id).bind(json!(account.external_identifiers))
                    .execute(&mut *session.tx).await?;
            } else {
                session.insert_account(&account).await?;
            }
            session.bump().await?;
        }
        let response = envelope(
            json!({"account":account,"previous":previous}),
            session.revision,
        );
        session
            .finish(
                WriteOperation {
                    principal,
                    tool: "sumi_accounts_upsert",
                    key: &request.idempotency_key,
                    request_hash: &request_hash,
                    request: request_value,
                },
                response,
                changed,
                Uuid::new_v4(),
            )
            .await
    }

    pub async fn transactions_apply(
        &self,
        principal: &Principal,
        request: TransactionsApply,
    ) -> Result<Value> {
        self.apply_complete(principal, request).await
    }

    pub async fn transactions_search(
        &self,
        principal: &Principal,
        request: TransactionsSearch,
    ) -> Result<Value> {
        let mut session = self.session(principal, request.ledger_id, false).await?;
        require(
            request
                .occurred_from
                .zip(request.occurred_before)
                .is_none_or(|(a, b)| a < b),
            "Date range must be [from,before)",
        )?;
        let filter_hash = hash(&(
            request.ledger_id,
            request.occurred_from,
            request.occurred_before,
            request.account_id,
            &request.merchant,
            &request.currency,
            request.amount_minor,
            request.category_id,
            request.tag_id,
            request.include_voided,
        ))?;
        let components = session.components().await?;
        let mut values = vec![];
        for t in session.current_transactions().await? {
            let data = &t.transaction;
            if (!request.include_voided && t.state != "posted")
                || request
                    .account_id
                    .is_some_and(|id| !data.financial_accounts().contains(&id))
                || request.merchant.as_ref().is_some_and(|m| {
                    data.merchant.as_ref() != Some(m)
                        && data.normalized_merchant.as_ref() != Some(m)
                })
                || request
                    .occurred_from
                    .is_some_and(|d| data.occurred_on.is_none_or(|v| v < d))
                || request
                    .occurred_before
                    .is_some_and(|d| data.occurred_on.is_none_or(|v| v >= d))
            {
                continue;
            }
            let mut monies = vec![];
            if let Some(m) = transactions::principal_money(data) {
                monies.push(m);
            }
            if let Movement::Transfer { to_money, .. } = &data.movement {
                monies.push(to_money.clone());
            }
            if let Movement::Refund { original_money, .. } = &data.movement {
                monies.push(original_money.clone());
            }
            monies.extend(data.fees.iter().map(|f| f.money.clone()));
            if request
                .currency
                .as_ref()
                .is_some_and(|c| !monies.iter().any(|m| &m.currency == c))
                || request
                    .amount_minor
                    .is_some_and(|a| !monies.iter().any(|m| m.amount_minor == a))
            {
                continue;
            }
            if request.category_id.is_some_and(|id| {
                !components
                    .iter()
                    .filter(|c| c.transaction_id == t.id)
                    .any(|c| c.allocations.iter().any(|a| a.category_id == Some(id)))
            }) {
                continue;
            }
            if request.tag_id.is_some_and(|id| {
                !data.tags.contains(&id)
                    && !components
                        .iter()
                        .filter(|c| c.transaction_id == t.id)
                        .any(|c| {
                            c.tags.contains(&id)
                                || c.allocations.iter().any(|a| a.tags.contains(&id))
                        })
            }) {
                continue;
            }
            values.push(json!({"id":t.id,"version":t.version,"state":t.state,"quality_flags":session.transaction_quality(&t).await?,"duplicate_candidates":session.duplicate_candidates(&t).await?,"transaction":data}));
        }
        let data = page(
            values,
            request.cursor.as_deref(),
            request.limit,
            session.revision,
            &filter_hash,
        )?;
        let response = envelope(data, session.revision);
        session.tx.commit().await?;
        Ok(response)
    }

    pub async fn transactions_get(
        &self,
        principal: &Principal,
        request: TransactionsGet,
    ) -> Result<Value> {
        let mut session = self.session(principal, request.ledger_id, false).await?;
        let current = session.get_current(request.transaction_id).await?;
        let rows=sqlx::query("SELECT version,data,state,reason,operation_id,recorded_revision,recorded_at FROM transaction_versions WHERE ledger_id=$1 AND transaction_id=$2 ORDER BY version")
            .bind(session.ledger_id).bind(request.transaction_id).fetch_all(&mut *session.tx).await?;
        let history=rows.into_iter().map(|r| Ok(json!({"version":r.try_get::<i64,_>("version")?,"transaction":r.try_get::<Value,_>("data")?,"state":r.try_get::<String,_>("state")?,"reason":r.try_get::<String,_>("reason")?,"operation_id":r.try_get::<Uuid,_>("operation_id")?,"recorded_revision":r.try_get::<i64,_>("recorded_revision")?,"recorded_at":r.try_get::<chrono::DateTime<chrono::Utc>,_>("recorded_at")?}))).collect::<Result<Vec<_>>>()?;
        let journal = session.journal_history(request.transaction_id).await?;
        let response = envelope(
            json!({"current":current,"history":history,"journal":journal,"quality_flags":session.transaction_quality(&current).await?,"evidence_history":session.evidence_links(Some(current.id),false).await?,"active_evidence":session.evidence_links(Some(current.id),true).await?,"refund_relationships":session.refund_relationships(current.id).await?,"duplicate_candidates":session.duplicate_candidates(&current).await?}),
            session.revision,
        );
        session.tx.commit().await?;
        Ok(response)
    }

    pub async fn operations_get(
        &self,
        principal: &Principal,
        request: OperationsGet,
    ) -> Result<Value> {
        let mut session = self.session(principal, request.ledger_id, false).await?;
        require(
            request.operation_id.is_some()
                || request.idempotency_key.is_some() && request.tool.is_some(),
            "Supply operation_id or tool plus idempotency_key",
        )?;
        let row=sqlx::query("SELECT id,tool,client_id,idempotency_key,request,result,recorded_at FROM operations WHERE ledger_id=$1 AND (($2::uuid IS NOT NULL AND id=$2) OR ($2::uuid IS NULL AND client_id=$3 AND tool=$4 AND idempotency_key=$5))")
            .bind(session.ledger_id).bind(request.operation_id).bind(&principal.client_id).bind(request.tool).bind(request.idempotency_key).fetch_optional(&mut *session.tx).await?.ok_or(Error::NotFound)?;
        let response = envelope(
            json!({"operation_id":row.try_get::<Uuid,_>("id")?,"tool":row.try_get::<String,_>("tool")?,"client_id":row.try_get::<String,_>("client_id")?,"idempotency_key":row.try_get::<String,_>("idempotency_key")?,"request":row.try_get::<Value,_>("request")?,"result":row.try_get::<Value,_>("result")?,"recorded_at":row.try_get::<chrono::DateTime<chrono::Utc>,_>("recorded_at")?}),
            session.revision,
        );
        session.tx.commit().await?;
        Ok(response)
    }

    pub async fn reports_get(&self, principal: &Principal, request: ReportsGet) -> Result<Value> {
        self.reports_complete(principal, request).await
    }
}

fn kind_text(kind: AccountKind) -> &'static str {
    match kind {
        AccountKind::Asset => "asset",
        AccountKind::Liability => "liability",
        AccountKind::Income => "income",
        AccountKind::Expense => "expense",
        AccountKind::Equity => "equity",
    }
}

fn quality(data: &TransactionData) -> Vec<&'static str> {
    let mut flags = vec![];
    if data.occurred_on.is_none() {
        flags.push("occurrence_date_unknown");
    }
    if matches!(
        data.movement,
        Movement::Expense { .. } | Movement::Income { .. }
    ) && data.allocations.iter().any(|a| a.category_id.is_none())
    {
        flags.push("uncategorized");
    }
    if matches!(
        data.movement,
        Movement::Expense {
            payment_account_id: None,
            ..
        }
    ) {
        flags.push("unknown_payment_account");
    }
    if data
        .fees
        .iter()
        .any(|f| f.allocations.iter().any(|a| a.category_id.is_none()))
    {
        flags.push("uncategorized_fee");
    }
    match data.movement {
        Movement::UnresolvedMovement {
            transfer_in_transit: true,
            ..
        } => flags.push("transfer_in_transit"),
        Movement::UnresolvedMovement { .. } => flags.push("unresolved_movement"),
        _ => {}
    }
    flags
}

fn opening_status(opening: Option<&(Uuid, NaiveDate)>, as_of: Option<NaiveDate>) -> Value {
    match opening {
        Some((id, date)) if as_of.is_none_or(|d| d >= *date) => {
            json!({"status":"confirmed","transaction_id":id,"as_of":date})
        }
        Some((id, date)) => {
            json!({"status":"outside_tracking_period","transaction_id":id,"as_of":date})
        }
        None => json!({"status":"unknown"}),
    }
}

fn validate_key(key: &str) -> Result<()> {
    require(
        !key.trim().is_empty() && key.len() <= 200,
        "idempotency_key must contain 1..200 bytes",
    )
}

#[derive(Serialize, Deserialize)]
struct Cursor {
    revision: i64,
    offset: usize,
    filter: String,
}
fn page(
    values: Vec<Value>,
    cursor: Option<&str>,
    limit: Option<u32>,
    revision: i64,
    filter: &str,
) -> Result<Value> {
    let limit = limit.unwrap_or(50);
    require((1..=100).contains(&limit), "Page limit must be 1..100")?;
    let offset = if let Some(cursor) = cursor {
        require(cursor.len() <= 512, "Invalid cursor")?;
        let c: Cursor = serde_json::from_str(cursor)?;
        if c.revision != revision {
            return Err(Error::CursorStale);
        }
        require(
            c.filter == filter && c.offset <= values.len(),
            "Cursor belongs to different query",
        )?;
        c.offset
    } else {
        0
    };
    let next = offset.saturating_add(limit as usize);
    let next_cursor = if next < values.len() {
        Some(serde_json::to_string(&Cursor {
            revision,
            offset: next,
            filter: filter.into(),
        })?)
    } else {
        None
    };
    Ok(
        json!({"items":values.into_iter().skip(offset).take(limit as usize).collect::<Vec<_>>(),"next_cursor":next_cursor}),
    )
}

impl Session {
    async fn accounts(&mut self) -> Result<BTreeMap<Uuid, Account>> {
        sqlx::query("SELECT id,name,kind,currency,role,active,version,parent_id,external_identifiers FROM accounts WHERE ledger_id=$1 ORDER BY id")
            .bind(self.ledger_id).fetch_all(&mut *self.tx).await?.into_iter().map(|r|account_row(r).map(|a|(a.id,a))).collect()
    }
    async fn insert_account(&mut self, account: &Account) -> Result<()> {
        sqlx::query("INSERT INTO accounts(ledger_id,id,name,kind,currency,role,active,version,parent_id,external_identifiers) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)")
            .bind(self.ledger_id).bind(account.id).bind(&account.name).bind(kind_text(account.kind)).bind(&account.currency).bind(&account.role).bind(account.active).bind(account.version).bind(account.parent_id).bind(json!(account.external_identifiers)).execute(&mut *self.tx).await?;
        Ok(())
    }
    async fn ensure_system_accounts(&mut self, entry: &PlannedEntry) -> Result<()> {
        for line in &entry.lines {
            for role in SYSTEM_ROLES {
                if system_account_id(self.ledger_id, role, &line.currency) == line.account_id {
                    sqlx::query("INSERT INTO accounts(ledger_id,id,name,kind,currency,role) VALUES($1,$2,$3,$4,$5,$6) ON CONFLICT(ledger_id,id) DO NOTHING")
                        .bind(self.ledger_id).bind(line.account_id).bind(format!("{role}:{}",line.currency)).bind(kind_text(system_kind(role))).bind(&line.currency).bind(role).execute(&mut *self.tx).await?;
                }
            }
        }
        Ok(())
    }
    async fn balances(&mut self, as_of: Option<NaiveDate>) -> Result<BTreeMap<Uuid, Amount>> {
        let rows=sqlx::query("SELECT l.account_id,sum(l.amount_minor)::text AS amount FROM journal_lines l JOIN journal_entries e ON e.ledger_id=l.ledger_id AND e.id=l.entry_id WHERE l.ledger_id=$1 AND ($2::date IS NULL OR e.effective_on<=$2) GROUP BY l.account_id")
            .bind(self.ledger_id).bind(as_of).fetch_all(&mut *self.tx).await?;
        rows.into_iter()
            .map(|r| {
                Ok((
                    r.try_get("account_id")?,
                    r.try_get::<String, _>("amount")?.parse()?,
                ))
            })
            .collect()
    }
    async fn openings(&mut self) -> Result<BTreeMap<Uuid, (Uuid, NaiveDate)>> {
        let mut result = BTreeMap::new();
        for t in self.current_transactions().await? {
            if t.state == "posted"
                && let Movement::Opening {
                    account_id, as_of, ..
                } = t.transaction.movement
            {
                result.insert(account_id, (t.id, as_of));
            }
        }
        Ok(result)
    }
    async fn current_transactions(&mut self) -> Result<Vec<CurrentTransaction>> {
        let rows=sqlx::query("SELECT t.id,t.current_version,t.state,v.data FROM transactions t JOIN transaction_versions v ON v.ledger_id=t.ledger_id AND v.transaction_id=t.id AND v.version=t.current_version WHERE t.ledger_id=$1 ORDER BY t.id")
            .bind(self.ledger_id).fetch_all(&mut *self.tx).await?;
        rows.into_iter().map(current_row).collect()
    }
    async fn get_current(&mut self, id: Uuid) -> Result<CurrentTransaction> {
        let row=sqlx::query("SELECT t.id,t.current_version,t.state,v.data FROM transactions t JOIN transaction_versions v ON v.ledger_id=t.ledger_id AND v.transaction_id=t.id AND v.version=t.current_version WHERE t.ledger_id=$1 AND t.id=$2")
            .bind(self.ledger_id).bind(id).fetch_optional(&mut *self.tx).await?.ok_or(Error::NotFound)?;
        current_row(row)
    }
    async fn active_entries(&mut self, id: Uuid) -> Result<Vec<(Uuid, PlannedEntry)>> {
        let rows=sqlx::query("SELECT e.id,e.effective_on FROM journal_entries e WHERE e.ledger_id=$1 AND e.transaction_id=$2 AND e.reverses_entry_id IS NULL AND NOT EXISTS(SELECT 1 FROM journal_entries r WHERE r.ledger_id=e.ledger_id AND r.reverses_entry_id=e.id) ORDER BY e.effective_on,e.id")
            .bind(self.ledger_id).bind(id).fetch_all(&mut *self.tx).await?;
        let mut entries = vec![];
        for row in rows {
            let id: Uuid = row.try_get("id")?;
            entries.push((
                id,
                PlannedEntry {
                    effective_on: row.try_get("effective_on")?,
                    lines: self.lines(id).await?,
                },
            ));
        }
        // Generation order must not affect financial equality (same-day FX has multiple entries).
        entries.sort_by_key(|(_, p)| (p.effective_on, p.lines.first().map(|l| l.account_id)));
        Ok(entries)
    }
    async fn lines(&mut self, id: Uuid) -> Result<Vec<JournalLine>> {
        sqlx::query("SELECT account_id,currency,amount_minor::text AS amount FROM journal_lines WHERE ledger_id=$1 AND entry_id=$2 ORDER BY line_number")
            .bind(self.ledger_id).bind(id).fetch_all(&mut *self.tx).await?.into_iter().map(|r| Ok(JournalLine{account_id:r.try_get("account_id")?,currency:r.try_get("currency")?,amount_minor:r.try_get::<String,_>("amount")?.parse()?})).collect()
    }
    async fn journal_history(&mut self, id: Uuid) -> Result<Vec<Value>> {
        let rows=sqlx::query("SELECT id,effective_on,transaction_version,recorded_revision,operation_id,reverses_entry_id FROM journal_entries WHERE ledger_id=$1 AND transaction_id=$2 ORDER BY recorded_revision,transaction_version,id")
            .bind(self.ledger_id).bind(id).fetch_all(&mut *self.tx).await?;
        let mut result = vec![];
        for row in rows {
            let id: Uuid = row.try_get("id")?;
            result.push(json!({"id":id,"effective_on":row.try_get::<NaiveDate,_>("effective_on")?,"transaction_version":row.try_get::<i64,_>("transaction_version")?,"recorded_revision":row.try_get::<i64,_>("recorded_revision")?,"operation_id":row.try_get::<Uuid,_>("operation_id")?,"reverses_entry_id":row.try_get::<Option<Uuid>,_>("reverses_entry_id")?,"lines":self.lines(id).await?}));
        }
        Ok(result)
    }
    async fn append_entry(
        &mut self,
        transaction_id: Uuid,
        transaction_version: i64,
        revision: i64,
        operation_id: Uuid,
        entry: &PlannedEntry,
        reverses: Option<Uuid>,
    ) -> Result<Uuid> {
        entry.validate()?;
        let id = Uuid::new_v4();
        sqlx::query("INSERT INTO journal_entries(ledger_id,id,transaction_id,transaction_version,effective_on,recorded_revision,operation_id,reverses_entry_id) VALUES($1,$2,$3,$4,$5,$6,$7,$8)")
            .bind(self.ledger_id).bind(id).bind(transaction_id).bind(transaction_version).bind(entry.effective_on).bind(revision).bind(operation_id).bind(reverses).execute(&mut *self.tx).await?;
        for (i, line) in entry.lines.iter().enumerate() {
            let amount: bigdecimal::BigDecimal = line
                .amount_minor
                .to_string()
                .parse()
                .map_err(|_| Error::Overflow)?;
            sqlx::query("INSERT INTO journal_lines(ledger_id,entry_id,line_number,account_id,currency,amount_minor) VALUES($1,$2,$3,$4,$5,$6)")
                .bind(self.ledger_id).bind(id).bind((i+1) as i32).bind(line.account_id).bind(&line.currency).bind(amount).execute(&mut *self.tx).await?;
        }
        Ok(id)
    }
    async fn bump(&mut self) -> Result<()> {
        self.revision = self.revision.checked_add(1).ok_or(Error::Overflow)?;
        sqlx::query("UPDATE ledgers SET revision=$2 WHERE id=$1")
            .bind(self.ledger_id)
            .bind(self.revision)
            .execute(&mut *self.tx)
            .await?;
        Ok(())
    }
    async fn replay(
        &mut self,
        principal: &Principal,
        tool: &str,
        key: &str,
        request_hash: &str,
    ) -> Result<Option<Value>> {
        validate_key(key)?;
        let row=sqlx::query("SELECT request_hash,result FROM operations WHERE ledger_id=$1 AND client_id=$2 AND tool=$3 AND idempotency_key=$4")
            .bind(self.ledger_id).bind(&principal.client_id).bind(tool).bind(key).fetch_optional(&mut *self.tx).await?;
        if let Some(row) = row {
            if row.try_get::<String, _>("request_hash")? != request_hash {
                return Err(Error::Idempotency);
            }
            Ok(Some(row.try_get("result")?))
        } else {
            Ok(None)
        }
    }
    async fn finish(
        mut self,
        operation: WriteOperation<'_>,
        mut response: Value,
        changed: bool,
        id: Uuid,
    ) -> Result<Value> {
        response["operation_id"] = json!(id);
        response["changed"] = json!(changed);
        sqlx::query("INSERT INTO operations(ledger_id,id,user_id,client_id,tool,idempotency_key,request_hash,request,result) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9)")
            .bind(self.ledger_id).bind(id).bind(operation.principal.user_id()).bind(&operation.principal.client_id).bind(operation.tool).bind(operation.key).bind(operation.request_hash).bind(operation.request).bind(&response).execute(&mut *self.tx).await?;
        self.tx.commit().await?;
        Ok(response)
    }
}

fn current_row(r: sqlx::postgres::PgRow) -> Result<CurrentTransaction> {
    let id: Uuid = r.try_get("id")?;
    let mut transaction: TransactionData = serde_json::from_value(r.try_get("data")?)?;
    preferences::normalize_allocations(id, &mut transaction)?;
    transaction.evidence_refs.get_or_insert_default();
    Ok(CurrentTransaction {
        id,
        version: r.try_get("current_version")?,
        state: r.try_get("state")?,
        transaction,
    })
}
