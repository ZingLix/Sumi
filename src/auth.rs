use crate::{
    config::{PublicUrl, validate_oauth_url},
    error::{Error, Result},
};
use axum::{
    extract::{Request, State},
    http::{StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use jsonwebtoken::{
    Algorithm, DecodingKey, Validation, decode, decode_header,
    jwk::{JwkSet, KeyAlgorithm, KeyOperations, PublicKeyUse},
};
use serde::Deserialize;
use serde_json::json;
use std::{collections::BTreeSet, sync::Arc};
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct Principal {
    pub issuer: String,
    pub subject: String,
    pub client_id: String,
    pub scopes: BTreeSet<String>,
}
impl Principal {
    pub fn user_id(&self) -> Uuid {
        Uuid::new_v5(
            &Uuid::NAMESPACE_URL,
            format!("{}\0{}", self.issuer, self.subject).as_bytes(),
        )
    }
    pub fn require(&self, scope: &str) -> Result<()> {
        if self.scopes.contains(scope) {
            Ok(())
        } else {
            Err(Error::Forbidden)
        }
    }
}

#[derive(Clone)]
pub struct Auth {
    issuer: String,
    public_url: PublicUrl,
    jwks: Arc<JwkSet>,
}

#[derive(Clone, Deserialize)]
struct Claims {
    iss: String,
    sub: String,
    scope: String,
    client_id: String,
    iat: u64,
    exp: u64,
}

impl Auth {
    pub fn new(issuer: String, resource: String, jwks: JwkSet) -> anyhow::Result<Self> {
        let issuer_url = url::Url::parse(&issuer)?;
        validate_oauth_url(&issuer_url)?;
        let public_url = PublicUrl::resolve(None, Some(&resource))?;
        anyhow::ensure!(
            !jwks.keys.is_empty(),
            "JWKS must contain a public signing key"
        );
        Ok(Self {
            issuer,
            public_url,
            jwks: Arc::new(jwks),
        })
    }
    pub fn validate(&self, token: &str) -> anyhow::Result<Principal> {
        anyhow::ensure!(token.len() <= 16 * 1024, "Token too large");
        let header = decode_header(token)?;
        anyhow::ensure!(
            matches!(header.alg, Algorithm::RS256 | Algorithm::ES256),
            "Unsupported signature algorithm"
        );
        anyhow::ensure!(
            header.typ.as_deref() == Some("at+jwt"),
            "An OAuth access token with typ=at+jwt is required"
        );
        let kid = header
            .kid
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("Missing kid"))?;
        let jwk = self
            .jwks
            .find(kid)
            .ok_or_else(|| anyhow::anyhow!("Unknown signing key"))?;
        anyhow::ensure!(
            jwk.common
                .public_key_use
                .as_ref()
                .is_none_or(|usage| *usage == PublicKeyUse::Signature),
            "Signing key is not intended for signatures"
        );
        anyhow::ensure!(
            jwk.common
                .key_operations
                .as_ref()
                .is_none_or(|ops| ops.contains(&KeyOperations::Verify)),
            "Signing key does not allow verification"
        );
        anyhow::ensure!(
            jwk.common.key_algorithm.is_none_or(|alg| matches!(
                (alg, header.alg),
                (KeyAlgorithm::RS256, Algorithm::RS256) | (KeyAlgorithm::ES256, Algorithm::ES256)
            )),
            "Signing key algorithm mismatch"
        );
        let key = DecodingKey::from_jwk(jwk)?;
        let mut validation = Validation::new(header.alg);
        validation.set_issuer(&[&self.issuer]);
        validation.set_audience(&[&self.public_url.resource_url]);
        validation.set_required_spec_claims(&["exp", "iss", "aud", "sub"]);
        validation.validate_nbf = true;
        validation.leeway = 30;
        let claims = decode::<Claims>(token, &key, &validation)?.claims;
        anyhow::ensure!(
            claims.iat <= jsonwebtoken::get_current_timestamp().saturating_add(30)
                && claims.exp > claims.iat,
            "Invalid token lifetime"
        );
        anyhow::ensure!(
            !claims.sub.is_empty() && claims.sub.len() <= 512 && !claims.sub.contains('\0'),
            "Invalid subject"
        );
        anyhow::ensure!(
            !claims.client_id.is_empty() && claims.client_id.len() <= 512,
            "Invalid client_id"
        );
        let scopes = claims
            .scope
            .split_ascii_whitespace()
            .map(str::to_owned)
            .collect::<BTreeSet<_>>();
        anyhow::ensure!(
            scopes.contains("sumi:read") || scopes.contains("sumi:write"),
            "No Sumi scope"
        );
        Ok(Principal {
            issuer: claims.iss,
            subject: claims.sub,
            client_id: claims.client_id,
            scopes,
        })
    }
    pub fn metadata(&self) -> serde_json::Value {
        json!({"resource":self.public_url.resource_url,"authorization_servers":[self.issuer],"scopes_supported":["sumi:read","sumi:write"],"bearer_methods_supported":["header"]})
    }
    pub fn public_url(&self) -> &PublicUrl {
        &self.public_url
    }
    fn challenge(&self) -> String {
        format!(
            "Bearer resource_metadata=\"{}{}\", scope=\"sumi:read sumi:write\"",
            self.public_url.origin, self.public_url.metadata_path
        )
    }
}

pub async fn authenticate(State(auth): State<Auth>, mut request: Request, next: Next) -> Response {
    if request
        .headers()
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        != Some(auth.public_url.authority.as_str())
    {
        return (StatusCode::BAD_REQUEST, "Invalid Host").into_response();
    }
    if let Some(origin) = request.headers().get(header::ORIGIN)
        && origin.to_str().ok() != Some(auth.public_url.origin.as_str())
    {
        return (StatusCode::FORBIDDEN, "Origin is not allowed").into_response();
    }
    let principal = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .and_then(|token| auth.validate(token).ok());
    match principal {
        Some(principal) => {
            request.extensions_mut().insert(principal);
            next.run(request).await
        }
        None => (
            StatusCode::UNAUTHORIZED,
            [(header::WWW_AUTHENTICATE, auth.challenge())],
            "A valid OAuth access token is required",
        )
            .into_response(),
    }
}
