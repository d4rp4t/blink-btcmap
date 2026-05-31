mod config {
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Clone, Deserialize, Serialize)]
    pub struct ServerConfig {
        #[serde(default = "default_port")]
        pub port: u16,
        #[serde(default = "default_jwks_url")]
        pub jwks_url: String,
    }

    impl Default for ServerConfig {
        fn default() -> Self {
            Self {
                port: default_port(),
                jwks_url: default_jwks_url(),
            }
        }
    }

    fn default_port() -> u16 {
        3000
    }

    fn default_jwks_url() -> String {
        "http://localhost:4456/.well-known/jwks.json".to_string()
    }
}

mod jwks {
    use axum::{async_trait, extract::FromRef, http::request::Parts, RequestPartsExt};
    use axum_extra::{
        headers::authorization::{Authorization, Bearer},
        TypedHeader,
    };
    use jsonwebtoken::{jwk::JwkSet, Algorithm, DecodingKey, TokenData, Validation};
    use serde::{de::DeserializeOwned, Deserialize};
    use std::sync::{Arc, RwLock};
    use thiserror::Error;


    #[derive(Error, Debug)]
    pub enum JwksError {
        #[error("JwksError - NoKeyAvailable")]
        NoKeyAvailable,
        #[error("JwksError - Jwt: {0}")]
        Jwt(#[from] jsonwebtoken::errors::Error),
        #[error("JwksError - Reqwest: {0}")]
        Reqwest(#[from] reqwest::Error),
    }

    pub enum AuthError {
        InvalidToken,
        MissingToken,
        ExpiredToken,
        InvalidSignature,
        InternalError,
    }

    impl axum::response::IntoResponse for AuthError {
        fn into_response(self) -> axum::response::Response {
            use axum::http::StatusCode;
            let (status, msg) = match self {
                AuthError::InvalidToken => (StatusCode::UNAUTHORIZED, "Invalid token"),
                AuthError::MissingToken => (StatusCode::UNAUTHORIZED, "Missing token"),
                AuthError::ExpiredToken => (StatusCode::UNAUTHORIZED, "Expired token"),
                AuthError::InvalidSignature => (StatusCode::UNAUTHORIZED, "Invalid signature"),
                AuthError::InternalError => (StatusCode::INTERNAL_SERVER_ERROR, "Internal error"),
            };
            (status, msg).into_response()
        }
    }


    #[derive(Debug, Deserialize)]
    pub struct Claims<T>(pub T);

    #[derive(Clone, FromRef)]
    pub struct JwtDecoderState {
        pub decoder: Arc<RemoteJwksDecoder>,
    }

    #[async_trait]
    impl<S, T> axum::extract::FromRequestParts<S> for Claims<T>
    where
        JwtDecoderState: FromRef<S>,
        S: Send + Sync,
        T: DeserializeOwned,
    {
        type Rejection = AuthError;

        async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
            let auth: TypedHeader<Authorization<Bearer>> = parts
                .extract()
                .await
                .map_err(|_| Self::Rejection::MissingToken)?;

            let state = JwtDecoderState::from_ref(state);
            let token_data = state.decoder.decode(auth.token()).map_err(|e| match e {
                JwksError::Jwt(e) => match e.kind() {
                    jsonwebtoken::errors::ErrorKind::ExpiredSignature => {
                        Self::Rejection::ExpiredToken
                    }
                    jsonwebtoken::errors::ErrorKind::InvalidSignature => {
                        Self::Rejection::InvalidSignature
                    }
                    _ => Self::Rejection::InvalidToken,
                },
                _ => Self::Rejection::InternalError,
            })?;

            Ok(token_data.claims)
        }
    }


    pub trait JwtDecoder<T>
    where
        T: for<'de> DeserializeOwned,
    {
        fn decode(&self, token: &str) -> Result<TokenData<T>, JwksError>;
    }

    pub struct RemoteJwksDecoder {
        jwks_url: String,
        cache_duration: std::time::Duration,
        keys_cache: RwLock<Vec<(Option<String>, DecodingKey)>>,
        validation: Validation,
        client: reqwest::Client,
        retry_count: usize,
        backoff: std::time::Duration,
    }

    impl RemoteJwksDecoder {
        pub fn new(jwks_url: String) -> Self {
            Self {
                jwks_url,
                cache_duration: std::time::Duration::from_secs(30 * 60),
                keys_cache: RwLock::new(Vec::new()),
                validation: Validation::new(Algorithm::RS256),
                client: reqwest::Client::new(),
                retry_count: 10,
                backoff: std::time::Duration::from_secs(2),
            }
        }

        pub async fn refresh_keys(&self) -> Result<(), JwksError> {
            let max_attempts = self.retry_count;
            let mut attempt = 0;
            let mut err = None;

            while attempt < max_attempts {
                match self.refresh_keys_once().await {
                    Ok(_) => return Ok(()),
                    Err(e) => {
                        err = Some(e);
                        attempt += 1;
                        tokio::time::sleep(self.backoff).await;
                    }
                }
            }

            Err(err.unwrap())
        }

        async fn refresh_keys_once(&self) -> Result<(), JwksError> {
            let jwks = self
                .client
                .get(&self.jwks_url)
                .send()
                .await?
                .json::<JwkSet>()
                .await?;

            let mut cache = self.keys_cache.write().unwrap();
            *cache = jwks
                .keys
                .iter()
                .flat_map(|jwk| -> Result<(Option<String>, DecodingKey), JwksError> {
                    let key_id = jwk.common.key_id.to_owned();
                    let key = DecodingKey::from_jwk(jwk).map_err(JwksError::Jwt)?;
                    Ok((key_id, key))
                })
                .collect();

            Ok(())
        }

        pub async fn refresh_keys_periodically(&self) {
            loop {
                match self.refresh_keys().await {
                    Ok(_) => {
                        tokio::time::sleep(self.cache_duration).await;
                    }
                    Err(e) => {
                        tracing::error!(
                            error = %e,
                            attempts = self.retry_count,
                            "Failed to refresh JWKS keys"
                        );
                        tokio::time::sleep(self.backoff).await;
                    }
                }
            }
        }
    }

    impl<T> JwtDecoder<T> for RemoteJwksDecoder
    where
        T: for<'de> DeserializeOwned,
    {
        fn decode(&self, token: &str) -> Result<TokenData<T>, JwksError> {
            let header = jsonwebtoken::decode_header(token)?;
            let target_kid = header.kid;

            let cache = self.keys_cache.read().unwrap();

            let jwk = cache.iter().find(|(kid, _)| kid == &target_kid);
            if let Some((_, key)) = jwk {
                return Ok(jsonwebtoken::decode::<T>(token, key, &self.validation)?);
            }

            let mut err = JwksError::NoKeyAvailable;
            for (_, key) in cache.iter() {
                match jsonwebtoken::decode::<T>(token, key, &self.validation) {
                    Ok(token_data) => return Ok(token_data),
                    Err(e) => err = e.into(),
                }
            }

            Err(err)
        }
    }
}


use async_graphql::*;
use async_graphql_axum::{GraphQLRequest, GraphQLResponse};
use axum::{routing::get, Extension, Router};
use axum_extra::headers::HeaderMap;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::instrument;

use crate::graphql;

pub use config::ServerConfig;
use jwks::*;

#[derive(Debug, Serialize, Deserialize)]
pub struct JwtClaims {
    sub: String,
    exp: u64,
    #[serde(default)]
    scope: String,
}

pub async fn run_server(
    config: ServerConfig,
    btcmap: Arc<crate::btcmap::BtcMapClient>,
) -> anyhow::Result<()> {
    let schema = graphql::schema(Some(btcmap));

    let jwks_decoder = Arc::new(RemoteJwksDecoder::new(config.jwks_url.clone()));
    jwks_decoder.refresh_keys().await?;
    let decoder = jwks_decoder.clone();
    tokio::spawn(async move {
        decoder.refresh_keys_periodically().await;
    });

    let app = Router::new()
        .route(
            "/graphql",
            get(playground).post(axum::routing::post(graphql_handler)),
        )
        .with_state(JwtDecoderState {
            decoder: jwks_decoder,
        })
        .layer(Extension(schema));

    tracing::info!("Starting graphql server on port {}", config.port);
    let listener =
        tokio::net::TcpListener::bind(&std::net::SocketAddr::from(([0, 0, 0, 0], config.port)))
            .await?;
    axum::serve(listener, app.into_make_service()).await?;
    Ok(())
}

#[instrument(name = "btcmap-proxy.graphql", skip_all, fields(sub))]
async fn graphql_handler(
    schema: Extension<Schema<graphql::Query, graphql::Mutation, EmptySubscription>>,
    Claims(jwt_claims): Claims<JwtClaims>,
    headers: HeaderMap,
    req: GraphQLRequest,
) -> GraphQLResponse {
    tracing::http::extract_tracing(&headers);
    tracing::Span::current().record("sub", &jwt_claims.sub);

    let req = req.into_inner();
    schema
        .execute(req.data(graphql::AuthSubject { id: jwt_claims.sub }))
        .await
        .into()
}

async fn playground() -> impl axum::response::IntoResponse {
    axum::response::Html(async_graphql::http::playground_source(
        async_graphql::http::GraphQLPlaygroundConfig::new("/graphql"),
    ))
}

#[cfg(test)]
mod tests {
    use super::jwks::{AuthError, Claims, JwksError, JwtDecoder, JwtDecoderState, RemoteJwksDecoder};
    use super::{graphql_handler, playground, run_server, JwtClaims, ServerConfig};
    use axum::{
        body::Body,
        http::{Request, StatusCode},
        response::IntoResponse,
        routing::{get, post},
        Extension, Router,
    };
    use serde_json::json;
    use std::sync::Arc;
    use std::time::{SystemTime, UNIX_EPOCH};
    use tower::ServiceExt;
    use wiremock::{matchers::method, Mock, MockServer, ResponseTemplate};

    // ── RSA test key material (2048-bit, test-only) ───────────────────────

    const TEST_PRIVATE_KEY_PEM: &[u8] = br"-----BEGIN RSA PRIVATE KEY-----
MIIEpAIBAAKCAQEA9231vZNJJR+YvqcIu8TIYyQWbJ1D3t3ob2YueY1PH6yV85Dm
NLgFnXvNdTeFEZbbcT5wo3MkdvEqckXZGFkUPVmdYmiRCzSeBXQjWwSVfVF5bClh
mBCTOY6YN/He7/azuquBsotTs7HTMZz1OKhFwFy+MPVlQY6/ZGrfE/qAUmdeDEag
WHWED86HYU7M8l9MfS15VGzBkf9Q554rducn4+XfJMAZFwizIeR0CqkbBdSxx296
hcpoZKWI8WrzsVM0bda9JE2i5uiKy7xijyv9jdPCT2qwUvNQlEK9YVFDCKeYuExO
CdPVUF7oFOLt48Wzzs0wPrRvDyIsG/rZVERO6QIDAQABAoIBAAJwmuT+7BB55olw
v7kMSHaTz0XMajQrJ4Tbstcfgdl72/GuKtr3upRyOVUv0jfZbzoHZdhyxPgISkUc
s7aWAElXliH3ioCcCPfgTI3z9l5pPIOIx+3WMgF2CsG4eJyQp/aOBOYkEhP6S60Y
UWG45REvyO9WKCS0meYNWLxLctL9LX1KyvzaOyhgpKeav0P0JfEUraVFIE5kURat
ATA2nj0KAY0wxR7fkDqqpUZYdWOwBpiEdTld2ucZGW8nvlhUUxXPDgPFX5bH4X8k
vZoKUQknh45HlvBJ4FIlIy0Yr6X6/bYdDRX2ROCS1ZNTCq8rwplb0pdLbWzIzoiL
MlwZMYECgYEA/O7R/c4FLO6TILztzvC9BbpoyMmZXuZc3qNl7FgBALl2iUcoXYTU
gXnjXKPFPrRFRBWD6tO1JmNvmzXrcjdtt4F3cnnCMsWDLz7tiFfBZPyoY/07IVQ2
BbsSlRXIbOs9Q9dj7srRFYIoeyp2164xAy6H9d7eWi+9kqukKw2oP8ECgYEA+m4O
ORy5BsVojBPUasOTERfjmOTWhjdSPMqrYGiOoISOmAptH9Wr4la/dLHD+l3fux8h
1a6vPw1kELBvXQDPorN40CHscljdxU64LP7mp276KjTQf+QsQ88BFqxxd3j3LerO
WLRsFbq35sMopFYV64DodEMcAsPFISxPlrAoWSkCgYEA0Lg+/0NAUBi7vptJXqiY
Qx7Vk0ORVZehcXPDCuqAQVnKcHQQ4kNXnVS5A1x9y0W1lv5uMpzrcrdBhQJUvZbx
6iljKUtCruUAYT97gjRweeZpCsIQRmuYfNgn+HDWSNNCZjZa19Xz/dy/jQu4sDil
Z2vBdGqqcB/PPzZ2rbSCb8ECgYAdPl7Q0obUwJatzN8APKhe1aBRSV+3upwS10Pd
9Te6jOAt5wHJNuVkf+bJlLyi7vViX4dO8aArR8AIpuHKRX75q+WOwHdg/vmewcuG
DZoXsUDrTtGOLbHxlSm2YRq67dhHd2TzPNZmTzCMdPu4/QiAQMRkVzXdKMlLT2ZX
3WhIyQKBgQDfcO+dBSQbH66iGJbUWCpBTsfeX9dC9Eq+Iw6zBc0F3fDfhQXvxMGU
ZqFG8tn+cxxoMKtjK6H5LVh9FB4T5LQdCFTEy4u8Wchb99OkS3lhDgoA3kUROy75
9VlV9HsArNQlvtQmiF6mKMQyQtXCz09OAe0bW5gr++U+FoSWsSsu+Q==
-----END RSA PRIVATE KEY-----";

    const TEST_KID: &str = "test-key-1";
    // RSA 2048 public key components (base64url, no padding)
    const TEST_N: &str = "9231vZNJJR-YvqcIu8TIYyQWbJ1D3t3ob2YueY1PH6yV85DmNLgFnXvNdTeFEZbbcT5wo3MkdvEqckXZGFkUPVmdYmiRCzSeBXQjWwSVfVF5bClhmBCTOY6YN_He7_azuquBsotTs7HTMZz1OKhFwFy-MPVlQY6_ZGrfE_qAUmdeDEagWHWED86HYU7M8l9MfS15VGzBkf9Q554rducn4-XfJMAZFwizIeR0CqkbBdSxx296hcpoZKWI8WrzsVM0bda9JE2i5uiKy7xijyv9jdPCT2qwUvNQlEK9YVFDCKeYuExOCdPVUF7oFOLt48Wzzs0wPrRvDyIsG_rZVERO6Q";
    const TEST_E: &str = "AQAB";

    fn jwks_body() -> serde_json::Value {
        json!({
            "keys": [{
                "kty": "RSA",
                "alg": "RS256",
                "use": "sig",
                "kid": TEST_KID,
                "n": TEST_N,
                "e": TEST_E
            }]
        })
    }

    fn future_exp() -> u64 {
        SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() + 3600
    }

    // Tamper a character in the middle of the JWT signature (avoid last chars which have
    // partial-byte padding that causes base64 decode errors instead of InvalidSignature)
    fn tamper_sig(token: &str) -> String {
        let parts: Vec<&str> = token.rsplitn(2, '.').collect();
        let sig = parts[0];
        let rest = parts[1];
        let mut chars: Vec<char> = sig.chars().collect();
        chars[5] = if chars[5] == 'A' { 'B' } else { 'A' };
        format!("{}.{}", rest, chars.into_iter().collect::<String>())
    }

    fn sign_token(exp: u64, kid: Option<&str>) -> String {
        let key = jsonwebtoken::EncodingKey::from_rsa_pem(TEST_PRIVATE_KEY_PEM).unwrap();
        let mut header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::RS256);
        header.kid = kid.map(|s| s.to_string());
        let claims = json!({ "sub": "user-test", "exp": exp });
        jsonwebtoken::encode(&header, &claims, &key).unwrap()
    }

    async fn decoder_with_keys(server: &MockServer) -> Arc<RemoteJwksDecoder> {
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_json(jwks_body()))
            .mount(server)
            .await;
        let d = Arc::new(RemoteJwksDecoder::new(server.uri() + "/jwks"));
        d.refresh_keys().await.unwrap();
        d
    }

    // ── ServerConfig ──────────────────────────────────────────────────────

    #[test]
    fn server_config_default_values() {
        let cfg = ServerConfig::default();
        assert_eq!(cfg.port, 3000);
        assert!(cfg.jwks_url.contains("jwks.json"));
    }

    // ── JwtClaims deserialization ─────────────────────────────────────────

    #[test]
    fn jwt_claims_default_scope() {
        let c: JwtClaims = serde_json::from_str(r#"{"sub":"u1","exp":9999}"#).unwrap();
        assert_eq!(c.sub, "u1");
        assert_eq!(c.scope, "");
    }

    #[test]
    fn jwt_claims_with_scope() {
        let c: JwtClaims = serde_json::from_str(r#"{"sub":"u1","exp":9999,"scope":"rw"}"#).unwrap();
        assert_eq!(c.scope, "rw");
    }

    // ── AuthError responses ───────────────────────────────────────────────

    #[tokio::test]
    async fn auth_error_invalid_token_response() {
        let r = AuthError::InvalidToken.into_response();
        assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
        let b = axum::body::to_bytes(r.into_body(), usize::MAX).await.unwrap();
        assert_eq!(&b[..], b"Invalid token");
    }

    #[tokio::test]
    async fn auth_error_missing_token_response() {
        let r = AuthError::MissingToken.into_response();
        assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
        let b = axum::body::to_bytes(r.into_body(), usize::MAX).await.unwrap();
        assert_eq!(&b[..], b"Missing token");
    }

    #[tokio::test]
    async fn auth_error_expired_token_response() {
        let r = AuthError::ExpiredToken.into_response();
        assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
        let b = axum::body::to_bytes(r.into_body(), usize::MAX).await.unwrap();
        assert_eq!(&b[..], b"Expired token");
    }

    #[tokio::test]
    async fn auth_error_invalid_signature_response() {
        let r = AuthError::InvalidSignature.into_response();
        assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
        let b = axum::body::to_bytes(r.into_body(), usize::MAX).await.unwrap();
        assert_eq!(&b[..], b"Invalid signature");
    }

    #[tokio::test]
    async fn auth_error_internal_error_response() {
        let r = AuthError::InternalError.into_response();
        assert_eq!(r.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let b = axum::body::to_bytes(r.into_body(), usize::MAX).await.unwrap();
        assert_eq!(&b[..], b"Internal error");
    }

    // ── JwksError display ─────────────────────────────────────────────────

    #[test]
    fn jwks_error_no_key_available_display() {
        assert!(JwksError::NoKeyAvailable.to_string().contains("NoKeyAvailable"));
    }

    #[test]
    fn jwks_error_jwt_display() {
        let e = jsonwebtoken::decode_header("not.a.jwt").unwrap_err();
        assert!(JwksError::Jwt(e).to_string().contains("Jwt"));
    }

    #[tokio::test]
    async fn jwks_error_reqwest_display() {
        let err = reqwest::Client::new()
            .get("http://127.0.0.1:1/jwks")
            .send()
            .await
            .unwrap_err();
        assert!(JwksError::Reqwest(err).to_string().contains("Reqwest"));
    }

    // ── RemoteJwksDecoder::decode ─────────────────────────────────────────

    #[tokio::test]
    async fn decode_empty_cache_returns_no_key_available() {
        let d = RemoteJwksDecoder::new("http://unused".into());
        let token = sign_token(future_exp(), None);
        let result: Result<jsonwebtoken::TokenData<serde_json::Value>, _> = d.decode(&token);
        assert!(matches!(result, Err(JwksError::NoKeyAvailable)));
    }

    #[tokio::test]
    async fn decode_invalid_token_format() {
        let d = RemoteJwksDecoder::new("http://unused".into());
        let result: Result<jsonwebtoken::TokenData<serde_json::Value>, _> = d.decode("not-a-jwt");
        assert!(matches!(result, Err(JwksError::Jwt(_))));
    }

    #[tokio::test]
    async fn decode_success_with_kid_match() {
        let server = MockServer::start().await;
        let d = decoder_with_keys(&server).await;
        let token = sign_token(future_exp(), Some(TEST_KID));
        let result: Result<jsonwebtoken::TokenData<serde_json::Value>, _> = d.decode(&token);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().claims["sub"], "user-test");
    }

    #[tokio::test]
    async fn decode_success_without_kid_via_fallback() {
        let server = MockServer::start().await;
        let d = decoder_with_keys(&server).await;
        // No kid in token → falls through to for-loop, still finds the key
        let token = sign_token(future_exp(), None);
        let result: Result<jsonwebtoken::TokenData<serde_json::Value>, _> = d.decode(&token);
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn decode_expired_token_with_kid_match() {
        let server = MockServer::start().await;
        let d = decoder_with_keys(&server).await;
        let token = sign_token(1, Some(TEST_KID)); // exp=1 is long past
        let result: Result<jsonwebtoken::TokenData<serde_json::Value>, _> = d.decode(&token);
        assert!(matches!(result, Err(JwksError::Jwt(_))));
    }

    #[tokio::test]
    async fn decode_tampered_signature_fallback_all_fail() {
        let server = MockServer::start().await;
        let d = decoder_with_keys(&server).await;
        let token = sign_token(future_exp(), None);
        let tampered = tamper_sig(&token);
        let result: Result<jsonwebtoken::TokenData<serde_json::Value>, _> = d.decode(&tampered);
        assert!(matches!(result, Err(JwksError::Jwt(_))));
    }

    // ── RemoteJwksDecoder::refresh_keys ──────────────────────────────────

    #[tokio::test]
    async fn refresh_keys_success() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_json(jwks_body()))
            .mount(&server)
            .await;
        let d = RemoteJwksDecoder::new(server.uri() + "/jwks");
        assert!(d.refresh_keys().await.is_ok());
    }

    #[tokio::test(start_paused = true)]
    async fn refresh_keys_exhausts_retries_on_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(500).set_body_raw(b"not-json", "text/plain"))
            .mount(&server)
            .await;

        let d = Arc::new(RemoteJwksDecoder::new(server.uri() + "/jwks"));
        let dc = d.clone();
        let handle = tokio::spawn(async move { dc.refresh_keys().await });

        for _ in 0..12 {
            tokio::task::yield_now().await;
            tokio::time::advance(std::time::Duration::from_secs(3)).await;
        }
        tokio::task::yield_now().await;
        let result = handle.await.unwrap();
        assert!(result.is_err());
    }

    // ── RemoteJwksDecoder::refresh_keys_periodically ─────────────────────

    #[tokio::test(start_paused = true)]
    async fn refresh_keys_periodically_success_branch() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_json(jwks_body()))
            .mount(&server)
            .await;

        let d = Arc::new(RemoteJwksDecoder::new(server.uri() + "/jwks"));
        let dc = d.clone();
        let handle = tokio::spawn(async move { dc.refresh_keys_periodically().await });

        tokio::task::yield_now().await; // runs refresh_keys (ok) → sleep(cache_duration)
        handle.abort();
        let _ = handle.await;
    }

    #[tokio::test(start_paused = true)]
    async fn refresh_keys_periodically_error_branch() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(500).set_body_raw(b"err", "text/plain"))
            .mount(&server)
            .await;

        let d = Arc::new(RemoteJwksDecoder::new(server.uri() + "/jwks"));
        let dc = d.clone();
        let handle = tokio::spawn(async move { dc.refresh_keys_periodically().await });

        for _ in 0..12 {
            tokio::task::yield_now().await;
            tokio::time::advance(std::time::Duration::from_secs(3)).await;
        }
        tokio::task::yield_now().await;
        handle.abort();
        let _ = handle.await;
    }

    // ── Claims::from_request_parts via axum test router ──────────────────

    async fn claims_handler(Claims(c): Claims<JwtClaims>) -> String {
        c.sub.clone()
    }

    fn test_app_with_decoder(decoder: Arc<RemoteJwksDecoder>) -> Router {
        Router::new()
            .route("/", get(claims_handler))
            .with_state(JwtDecoderState { decoder })
    }

    #[tokio::test]
    async fn claims_extractor_missing_header() {
        let server = MockServer::start().await;
        let d = decoder_with_keys(&server).await;
        let app = test_app_with_decoder(d);
        let req = Request::builder().uri("/").body(Body::empty()).unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn claims_extractor_valid_token() {
        let server = MockServer::start().await;
        let d = decoder_with_keys(&server).await;
        let token = sign_token(future_exp(), Some(TEST_KID));
        let app = test_app_with_decoder(d);
        let req = Request::builder()
            .uri("/")
            .header("Authorization", format!("Bearer {}", token))
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        assert_eq!(&body[..], b"user-test");
    }

    #[tokio::test]
    async fn claims_extractor_expired_token() {
        let server = MockServer::start().await;
        let d = decoder_with_keys(&server).await;
        let token = sign_token(1, Some(TEST_KID)); // expired
        let app = test_app_with_decoder(d);
        let req = Request::builder()
            .uri("/")
            .header("Authorization", format!("Bearer {}", token))
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        assert_eq!(&body[..], b"Expired token");
    }

    #[tokio::test]
    async fn claims_extractor_invalid_signature() {
        let server = MockServer::start().await;
        let d = decoder_with_keys(&server).await;
        let token = sign_token(future_exp(), Some(TEST_KID));
        let tampered = tamper_sig(&token);

        let app = test_app_with_decoder(d);
        let req = Request::builder()
            .uri("/")
            .header("Authorization", format!("Bearer {}", tampered))
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        assert_eq!(&body[..], b"Invalid signature");
    }

    #[tokio::test]
    async fn claims_extractor_malformed_token() {
        let server = MockServer::start().await;
        let d = decoder_with_keys(&server).await;
        let app = test_app_with_decoder(d);
        let req = Request::builder()
            .uri("/")
            .header("Authorization", "Bearer not.a.real.jwt")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        assert_eq!(&body[..], b"Invalid token");
    }

    #[tokio::test]
    async fn claims_extractor_internal_error_empty_cache() {
        // No refresh_keys → empty cache → NoKeyAvailable → InternalError
        let d = Arc::new(RemoteJwksDecoder::new("http://unused".into()));
        let token = sign_token(future_exp(), Some(TEST_KID));
        let app = test_app_with_decoder(d);
        let req = Request::builder()
            .uri("/")
            .header("Authorization", format!("Bearer {}", token))
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    // ── playground handler ────────────────────────────────────────────────

    #[tokio::test]
    async fn playground_returns_html() {
        let resp = playground().await.into_response();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        assert!(body.len() > 100);
        assert!(std::str::from_utf8(&body).unwrap().contains("GraphQL"));
    }

    // ── graphql_handler via test Router ──────────────────────────────────

    #[tokio::test]
    async fn graphql_handler_executes_query() {
        let server = MockServer::start().await;
        let d = decoder_with_keys(&server).await;
        let token = sign_token(future_exp(), Some(TEST_KID));
        let schema = crate::graphql::schema(None);

        let app = Router::new()
            .route(
                "/graphql",
                post(graphql_handler),
            )
            .with_state(JwtDecoderState { decoder: d })
            .layer(Extension(schema));

        let req = Request::builder()
            .uri("/graphql")
            .method("POST")
            .header("Authorization", format!("Bearer {}", token))
            .header("Content-Type", "application/json")
            .body(Body::from(r#"{"query":"{ __typename }"}"#))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    // ── run_server error path ─────────────────────────────────────────────

    #[tokio::test(start_paused = true)]
    async fn run_server_fails_if_jwks_unreachable() {
        let cfg = ServerConfig {
            port: 0,
            jwks_url: "http://127.0.0.1:1/jwks".into(),
        };
        let btcmap = Arc::new(crate::btcmap::BtcMapClient::new(
            "http://unused".into(),
            "key".into(),
            "blink".into(),
        ));
        let handle = tokio::spawn(async move { run_server(cfg, btcmap).await });
        for _ in 0..12 {
            tokio::task::yield_now().await;
            tokio::time::advance(std::time::Duration::from_secs(3)).await;
        }
        tokio::task::yield_now().await;
        let result = handle.await.unwrap();
        assert!(result.is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn run_server_starts_successfully() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_json(jwks_body()))
            .mount(&server)
            .await;

        let cfg = ServerConfig {
            port: 0,
            jwks_url: server.uri() + "/jwks",
        };
        let btcmap = Arc::new(crate::btcmap::BtcMapClient::new(
            "http://unused".into(),
            "key".into(),
            "blink".into(),
        ));
        let handle = tokio::spawn(async move { run_server(cfg, btcmap).await });
        tokio::task::yield_now().await;
        tokio::time::advance(std::time::Duration::from_millis(50)).await;
        tokio::task::yield_now().await;
        handle.abort();
        let _ = handle.await;
    }
}
