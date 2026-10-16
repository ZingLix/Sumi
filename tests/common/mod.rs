use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use serde_json::{Value, json};
use sumi::auth::{Auth, Principal};

pub fn principal(subject: &str) -> Principal {
    Principal {
        issuer: "https://auth.sumi.test/".into(),
        subject: subject.into(),
        client_id: "test-client".into(),
        scopes: ["sumi:read".into(), "sumi:write".into()]
            .into_iter()
            .collect(),
    }
}
pub fn auth() -> Auth {
    Auth::new(
        "https://auth.sumi.test/".into(),
        "http://127.0.0.1:8080/mcp".into(),
        serde_json::from_str(include_str!("../fixtures/jwks.json")).unwrap(),
    )
    .unwrap()
}
pub fn claims(subject: &str) -> Value {
    let now = chrono::Utc::now().timestamp();
    json!({"iss":"https://auth.sumi.test/","sub":subject,"aud":"http://127.0.0.1:8080/mcp","scope":"sumi:read sumi:write","client_id":"test-client","iat":now,"exp":now+300})
}
pub fn token(claims: &Value) -> String {
    let mut header = Header::new(Algorithm::RS256);
    header.typ = Some("at+jwt".into());
    header.kid = Some("sumi-test-only".into());
    encode(
        &header,
        claims,
        &EncodingKey::from_rsa_pem(include_bytes!("../fixtures/test-key.pem")).unwrap(),
    )
    .unwrap()
}
