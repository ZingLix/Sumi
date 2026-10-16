use crate::{
    api::*,
    auth::{Auth, Principal, authenticate},
    error::Result as AppResult,
    store::Store,
};
use axum::{Router, extract::State, middleware, routing::get};
use rmcp::{
    RoleServer, ServerHandler,
    handler::server::wrapper::Parameters,
    model::*,
    service::RequestContext,
    tool, tool_handler, tool_router,
    transport::streamable_http_server::{
        StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
    },
};
use tower_http::{limit::RequestBodyLimitLayer, trace::TraceLayer};

#[derive(Clone)]
pub struct SumiMcp {
    store: Store,
}

fn principal(ctx: &RequestContext<RoleServer>) -> AppResult<Principal> {
    ctx.extensions
        .get::<axum::http::request::Parts>()
        .and_then(|p| p.extensions.get::<Principal>())
        .cloned()
        .ok_or(crate::error::Error::Forbidden)
}

fn result(value: AppResult<serde_json::Value>) -> CallToolResult {
    match value {
        Ok(value) => CallToolResult::structured(value),
        Err(error) => {
            let mut response =
                CallToolResult::structured(serde_json::json!({"error":error.body()}));
            response.is_error = Some(true);
            response
        }
    }
}

#[tool_router]
impl SumiMcp {
    pub fn new(store: Store) -> Self {
        Self { store }
    }

    #[tool(
        name = "sumi_context_get",
        description = "Get or initialize your personal ledger, preferences, categories, currencies, unresolved observations, review notes and scoped reconciliation results.",
        annotations(
            read_only_hint = false,
            idempotent_hint = true,
            destructive_hint = false
        )
    )]
    async fn context_get(
        &self,
        ctx: RequestContext<RoleServer>,
        Parameters(_request): Parameters<ContextRequest>,
    ) -> CallToolResult {
        result(async { self.store.context(&principal(&ctx)?).await }.await)
    }
    #[tool(
        name = "sumi_accounts_list",
        description = "List real accounts, original-currency balances and opening baseline status. Debit is positive; credit card debt is negative.",
        annotations(read_only_hint = true)
    )]
    async fn accounts_list(
        &self,
        ctx: RequestContext<RoleServer>,
        Parameters(request): Parameters<AccountsList>,
    ) -> CallToolResult {
        result(async { self.store.accounts_list(&principal(&ctx)?, request).await }.await)
    }
    #[tool(
        name = "sumi_accounts_upsert",
        description = "Create or update a real asset or liability account. Updates require expected_version. Balances must be set through set_opening, never by editing an account.",
        annotations(idempotent_hint = true, destructive_hint = false)
    )]
    async fn accounts_upsert(
        &self,
        ctx: RequestContext<RoleServer>,
        Parameters(request): Parameters<AccountsUpsert>,
    ) -> CallToolResult {
        result(async { self.store.accounts_upsert(&principal(&ctx)?, request).await }.await)
    }
    #[tool(
        name = "sumi_transactions_apply",
        description = "Atomically create, revise, void, link/unlink evidence or set opening balances (up to 50 actions). Supports income, expense, transfer, refund, fees and unresolved movements. Values are integer minor-unit strings. Corrections append reversals on original dates. Preview rolls back. Read versions before modifying; preserve allocation IDs when revising.",
        annotations(idempotent_hint = true, destructive_hint = false)
    )]
    async fn transactions_apply(
        &self,
        ctx: RequestContext<RoleServer>,
        Parameters(request): Parameters<TransactionsApply>,
    ) -> CallToolResult {
        result(
            async {
                self.store
                    .transactions_apply(&principal(&ctx)?, request)
                    .await
            }
            .await,
        )
    }
    #[tool(
        name = "sumi_transactions_search",
        description = "Search current transaction versions with occurrence-date, account and merchant filters. Pagination fails with CURSOR_STALE if the ledger changed.",
        annotations(read_only_hint = true)
    )]
    async fn transactions_search(
        &self,
        ctx: RequestContext<RoleServer>,
        Parameters(request): Parameters<TransactionsSearch>,
    ) -> CallToolResult {
        result(
            async {
                self.store
                    .transactions_search(&principal(&ctx)?, request)
                    .await
            }
            .await,
        )
    }
    #[tool(
        name = "sumi_transactions_get",
        description = "Read a transaction's current version, full revision history, immutable journal and evidence quality flags.",
        annotations(read_only_hint = true)
    )]
    async fn transactions_get(
        &self,
        ctx: RequestContext<RoleServer>,
        Parameters(request): Parameters<TransactionsGet>,
    ) -> CallToolResult {
        result(
            async {
                self.store
                    .transactions_get(&principal(&ctx)?, request)
                    .await
            }
            .await,
        )
    }
    #[tool(
        name = "sumi_operations_get",
        description = "Recover the committed result and request for an operation in your own ledger.",
        annotations(read_only_hint = true)
    )]
    async fn operations_get(
        &self,
        ctx: RequestContext<RoleServer>,
        Parameters(request): Parameters<OperationsGet>,
    ) -> CallToolResult {
        result(async { self.store.operations_get(&principal(&ctx)?, request).await }.await)
    }
    #[tool(
        name = "sumi_reports_get",
        description = "Get current income/expense, category totals, balances, data quality or a complete consistent export. Supports scoped filters and display currency with quote provenance and missing items. Refunds inherit original current categories; FX allocation precedes filters.",
        annotations(read_only_hint = true)
    )]
    async fn reports_get(
        &self,
        ctx: RequestContext<RoleServer>,
        Parameters(request): Parameters<ReportsGet>,
    ) -> CallToolResult {
        result(async { self.store.reports_get(&principal(&ctx)?, request).await }.await)
    }
    #[tool(
        name = "sumi_sources_ingest",
        description = "Archive up to 100 text or JSON sources (512 KiB per body), preserving immutable versions and server hashes.",
        annotations(idempotent_hint = true, destructive_hint = false)
    )]
    async fn sources_ingest(
        &self,
        ctx: RequestContext<RoleServer>,
        Parameters(request): Parameters<SourcesIngest>,
    ) -> CallToolResult {
        result(async { self.store.sources_ingest(&principal(&ctx)?, request).await }.await)
    }

    #[tool(
        name = "sumi_sources_get",
        description = "Read an archived source version and its observation positions. Capture level is submitter-declared.",
        annotations(read_only_hint = true)
    )]
    async fn sources_get(
        &self,
        ctx: RequestContext<RoleServer>,
        Parameters(request): Parameters<SourcesGet>,
    ) -> CallToolResult {
        result(async { self.store.sources_get(&principal(&ctx)?, request).await }.await)
    }

    #[tool(
        name = "sumi_observations_ingest",
        description = "Atomically ingest up to 100 observations, update extraction versions or review notes, and track import batches. Changed evidence stays allocated and requires review; posted money is preserved.",
        annotations(idempotent_hint = true, destructive_hint = false)
    )]
    async fn observations_ingest(
        &self,
        ctx: RequestContext<RoleServer>,
        Parameters(request): Parameters<ObservationsIngest>,
    ) -> CallToolResult {
        result(
            async {
                self.store
                    .observations_ingest(&principal(&ctx)?, request)
                    .await
            }
            .await,
        )
    }

    #[tool(
        name = "sumi_observations_search",
        description = "Search observations, current evidence occupancy, source conflicts, notes and import batch progress.",
        annotations(read_only_hint = true)
    )]
    async fn observations_search(
        &self,
        ctx: RequestContext<RoleServer>,
        Parameters(request): Parameters<ObservationsSearch>,
    ) -> CallToolResult {
        result(
            async {
                self.store
                    .observations_search(&principal(&ctx)?, request)
                    .await
            }
            .await,
        )
    }

    #[tool(
        name = "sumi_fx_rates_ingest",
        description = "Save immutable dated decimal quotes backed by archived source versions. Does not rewrite actual settlements.",
        annotations(idempotent_hint = true, destructive_hint = false)
    )]
    async fn fx_rates_ingest(
        &self,
        ctx: RequestContext<RoleServer>,
        Parameters(request): Parameters<FxRatesIngest>,
    ) -> CallToolResult {
        result(async { self.store.fx_rates_ingest(&principal(&ctx)?, request).await }.await)
    }

    #[tool(
        name = "sumi_fx_rates_query",
        description = "Query dated FX quotes and their sources. Rates mean 1 from_currency = rate to_currency.",
        annotations(read_only_hint = true)
    )]
    async fn fx_rates_query(
        &self,
        ctx: RequestContext<RoleServer>,
        Parameters(request): Parameters<FxRatesQuery>,
    ) -> CallToolResult {
        result(async { self.store.fx_rates_query(&principal(&ctx)?, request).await }.await)
    }

    #[tool(
        name = "sumi_preferences_update",
        description = "Update versioned categories, tags, merchant aliases, classification rules, display currency and timezone. Timezone freezes after posting.",
        annotations(idempotent_hint = true, destructive_hint = false)
    )]
    async fn preferences_update(
        &self,
        ctx: RequestContext<RoleServer>,
        Parameters(request): Parameters<PreferencesUpdate>,
    ) -> CallToolResult {
        result(
            async {
                self.store
                    .preferences_update(&principal(&ctx)?, request)
                    .await
            }
            .await,
        )
    }

    #[tool(
        name = "sumi_reconcile",
        description = "Compare original-currency balances and acquired statement movements bidirectionally. Returns independent reconciliation and import_completeness results with scope and revision.",
        annotations(read_only_hint = true)
    )]
    async fn reconcile(
        &self,
        ctx: RequestContext<RoleServer>,
        Parameters(request): Parameters<ReconcileRequest>,
    ) -> CallToolResult {
        result(async { self.store.reconcile(&principal(&ctx)?, request).await }.await)
    }
}

#[tool_handler]
impl ServerHandler for SumiMcp {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("sumi",env!("CARGO_PKG_VERSION")))
            .with_instructions("Personal ledger for AI. Call context first. Preserve source and observation identities, use idempotency keys and current entity versions, and read notes before correction. posted only means balanced and committed. Always report reconciliation and import_completeness separately with scope and staleness. Resolve stale evidence atomically; do not create another transaction from a newer observation version. Similar date/merchant/amount only means duplicate candidate.")
    }
}

pub fn router(store: Store, auth: Auth) -> Router {
    let public_url = auth.public_url().clone();
    let service = StreamableHttpService::new(
        move || Ok(SumiMcp::new(store.clone())),
        LocalSessionManager::default().into(),
        StreamableHttpServerConfig::default()
            .with_allowed_hosts([public_url.authority.clone()])
            .with_allowed_origins([public_url.origin.clone()])
            .with_stateful_mode(false)
            .with_json_response(true),
    );
    let protected = Router::new()
        .nest_service(&public_url.mcp_path, service)
        .layer(middleware::from_fn_with_state(auth.clone(), authenticate));
    let mut router = Router::new()
        .merge(protected)
        .route(
            &public_url.metadata_path,
            get(|State(auth): State<Auth>| async move { axum::Json(auth.metadata()) }),
        )
        .route(
            "/.well-known/oauth-protected-resource",
            get(|State(auth): State<Auth>| async move { axum::Json(auth.metadata()) }),
        )
        .route("/healthz", get(|| async { "ok" }));
    if public_url.health_path != "/healthz" {
        router = router.route(&public_url.health_path, get(|| async { "ok" }));
    }
    router
        .with_state(auth)
        .layer(RequestBodyLimitLayer::new(1024 * 1024))
        .layer(TraceLayer::new_for_http())
}
