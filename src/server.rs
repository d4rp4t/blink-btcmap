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
