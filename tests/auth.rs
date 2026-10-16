mod common;
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use serde_json::json;
use sumi::{server, store::Store};
use tower::ServiceExt;

#[test]
fn access_tokens_validate_signature_issuer_audience_expiration_and_scopes() {
    let auth = common::auth();
    let claims = common::claims("alice");
    let validated = auth.validate(&common::token(&claims)).unwrap();
    assert_eq!(validated.user_id(), common::principal("alice").user_id());
    assert_ne!(validated.user_id(), common::principal("bob").user_id());
    for (field, bad) in [
        ("iss", json!("https://other.test/")),
        ("aud", json!("https://other.test/mcp")),
        ("exp", json!(1)),
        ("iat", json!(chrono::Utc::now().timestamp() + 3600)),
        ("scope", json!("profile")),
        ("sub", json!("")),
        ("client_id", json!("")),
        ("nbf", json!(chrono::Utc::now().timestamp() + 3600)),
    ] {
        let mut invalid = claims.clone();
        invalid[field] = bad;
        assert!(
            auth.validate(&common::token(&invalid)).is_err(),
            "accepted invalid {field}"
        );
    }
    let mut invalid = claims.clone();
    invalid.as_object_mut().unwrap().remove("exp");
    assert!(auth.validate(&common::token(&invalid)).is_err());
    invalid = claims.clone();
    invalid.as_object_mut().unwrap().remove("iat");
    assert!(auth.validate(&common::token(&invalid)).is_err());
    let token = common::token(&claims);
    let mut pieces = token.split('.').map(str::to_owned).collect::<Vec<_>>();
    pieces[1] = "eyJzdWIiOiJhZG1pbiJ9".into();
    assert!(auth.validate(&pieces.join(".")).is_err());
}

#[tokio::test]
async fn http_requires_bearer_and_validates_origin_and_host() {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .connect_lazy("postgres://unused@127.0.0.1/unused")
        .unwrap();
    let router = server::router(Store::new(pool), common::auth());
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/mcp")
                .header("host", "127.0.0.1:8080")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert!(
        response.headers()["www-authenticate"]
            .to_str()
            .unwrap()
            .contains("oauth-protected-resource/mcp")
    );
    let token = common::token(&common::claims("alice"));
    for (host, origin, status) in [
        ("127.0.0.1:8080", "https://evil.test", StatusCode::FORBIDDEN),
        (
            "evil.test",
            "http://127.0.0.1:8080",
            StatusCode::BAD_REQUEST,
        ),
    ] {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/mcp")
                    .header("host", host)
                    .header("origin", origin)
                    .header("authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), status);
    }
    let response = router
        .oneshot(
            Request::builder()
                .uri("/.well-known/oauth-protected-resource/mcp")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn public_subpath_drives_routes_discovery_and_token_audience() {
    let resource = "https://ledger.example.com:8443/apps/sumi/mcp";
    let auth = sumi::auth::Auth::new(
        "https://auth.sumi.test/".into(),
        resource.into(),
        serde_json::from_str(include_str!("fixtures/jwks.json")).unwrap(),
    )
    .unwrap();
    let mut claims = common::claims("alice");
    assert!(auth.validate(&common::token(&claims)).is_err());
    claims["aud"] = json!(resource);
    let token = common::token(&claims);
    assert!(auth.validate(&token).is_ok());
    let pool = sqlx::postgres::PgPoolOptions::new()
        .connect_lazy("postgres://unused@127.0.0.1/unused")
        .unwrap();
    let router = server::router(Store::new(pool), auth);
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/apps/sumi/mcp")
                .header("host", "ledger.example.com:8443")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        response.headers()["www-authenticate"],
        "Bearer resource_metadata=\"https://ledger.example.com:8443/.well-known/oauth-protected-resource/apps/sumi/mcp\", scope=\"sumi:read sumi:write\""
    );
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/.well-known/oauth-protected-resource/apps/sumi/mcp")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let metadata: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(metadata["resource"], resource);
    for path in ["/healthz", "/apps/sumi/healthz"] {
        let response = router
            .clone()
            .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }
    for (host, origin, forwarded, status) in [
        (
            "internal:8080",
            "https://ledger.example.com:8443",
            "ledger.example.com:8443",
            StatusCode::BAD_REQUEST,
        ),
        (
            "ledger.example.com:8443",
            "https://evil.example.com",
            "ledger.example.com:8443",
            StatusCode::FORBIDDEN,
        ),
    ] {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/apps/sumi/mcp")
                    .header("host", host)
                    .header("origin", origin)
                    .header("x-forwarded-host", forwarded)
                    .header("authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), status);
    }
    let response = router.clone().oneshot(Request::builder().method("POST").uri("/apps/sumi/mcp").header("host", "ledger.example.com:8443").header("origin", "https://ledger.example.com:8443").header("authorization", format!("Bearer {token}")).header("content-type", "application/json").header("accept", "application/json, text/event-stream").body(Body::from(json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"subpath-test","version":"1"}}}).to_string())).unwrap()).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let initialized: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(initialized["result"]["serverInfo"]["name"], "sumi");
    let response = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/mcp")
                .header("host", "ledger.example.com:8443")
                .header("authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}
