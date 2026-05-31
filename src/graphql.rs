use async_graphql::*;
use std::sync::Arc;
use uuid::Uuid;

use crate::btcmap::BtcMapClient;

pub struct AuthSubject {
    pub id: String,
}

#[derive(SimpleObject)]
#[graphql(extends, complex)]
struct User {
    #[graphql(external)]
    id: ID,
}

#[ComplexObject]
impl User {}

pub struct Query;

#[Object]
impl Query {
    #[graphql(entity)]
    async fn me(&self, id: ID) -> Option<User> {
        Some(User { id })
    }
}

#[derive(InputObject)]
pub struct BtcMapSubmitPlaceInput {
    pub lat: f64,
    pub lon: f64,
    /// single-word lowercase category (e.g. restaurant, atm, hotel)
    pub category: String,
    pub name: String,
    pub website: Option<String>,
    pub opening_hours: Option<String>,
    pub phone: Option<String>,
    pub description: Option<String>,
}

#[derive(SimpleObject)]
pub struct BtcMapSubmittedPlace {
    pub id: i64,
    pub origin: String,
    pub external_id: String,
}

#[derive(SimpleObject)]
pub struct BtcMapSubmitPlacePayload {
    pub place: BtcMapSubmittedPlace,
}

#[derive(InputObject)]
pub struct BtcMapVerifyElementInput {
    /// OSM element ID in the form "node:12345678" or "way:12345678"
    pub element_id: String,
}

#[derive(SimpleObject)]
pub struct BtcMapVerifyElementPayload {
    pub success: bool,
}

pub struct Mutation;

#[Object]
impl Mutation {
    /// submit a new Bitcoin-accepting place to BTC Map.
    async fn btcmap_submit_place(
        &self,
        ctx: &Context<'_>,
        input: BtcMapSubmitPlaceInput,
    ) -> Result<BtcMapSubmitPlacePayload> {
        validate_submit_place(&input)?;

        let subject = ctx.data::<AuthSubject>()?;
        let btcmap = ctx.data_unchecked::<Arc<BtcMapClient>>();

        let extra_fields = build_extra_fields(
            input.website.as_deref(),
            input.opening_hours.as_deref(),
            input.phone.as_deref(),
            input.description.as_deref(),
        );

        let external_id = format!("{}:{}", subject.id, Uuid::new_v4());

        let place = btcmap
            .submit_place(
                &external_id,
                input.lat,
                input.lon,
                &input.category,
                &input.name,
                extra_fields,
            )
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "btcmap submit_place failed");
                Error::new("Failed to submit place")
            })?;

        Ok(BtcMapSubmitPlacePayload {
            place: BtcMapSubmittedPlace {
                id: place.id,
                origin: place.origin,
                external_id: place.external_id,
            },
        })
    }

    /// verify that a place still accepts Bitcoin by recording today's survey date.
    /// requires the btcmap admin API key to have element_admin role.
    async fn btcmap_verify_element(
        &self,
        ctx: &Context<'_>,
        input: BtcMapVerifyElementInput,
    ) -> Result<BtcMapVerifyElementPayload> {
        ctx.data::<AuthSubject>()?;
        let btcmap = ctx.data_unchecked::<Arc<BtcMapClient>>();

        btcmap
            .verify_element(&input.element_id)
            .await
            .map_err(|e| {
                tracing::error!(error = %e, element_id = %input.element_id, "btcmap verify_element failed");
                Error::new("Failed to verify element")
            })?;

        Ok(BtcMapVerifyElementPayload { success: true })
    }
}

fn validate_submit_place(input: &BtcMapSubmitPlaceInput) -> Result<()> {
    if !(-90.0..=90.0).contains(&input.lat) {
        return Err(Error::new("Latitude must be between -90 and 90"));
    }
    if !(-180.0..=180.0).contains(&input.lon) {
        return Err(Error::new("Longitude must be between -180 and 180"));
    }
    if input.name.trim().is_empty() {
        return Err(Error::new("Name cannot be empty"));
    }
    if input.category.trim().is_empty() || input.category.contains(' ') {
        return Err(Error::new("Category must be a single lowercase word"));
    }
    Ok(())
}

fn build_extra_fields(
    website: Option<&str>,
    opening_hours: Option<&str>,
    phone: Option<&str>,
    description: Option<&str>,
) -> Option<serde_json::Value> {
    let mut map = serde_json::Map::new();
    if let Some(v) = website {
        map.insert("website".into(), serde_json::Value::String(v.into()));
    }
    if let Some(v) = opening_hours {
        map.insert("opening_hours".into(), serde_json::Value::String(v.into()));
    }
    if let Some(v) = phone {
        map.insert("phone".into(), serde_json::Value::String(v.into()));
    }
    if let Some(v) = description {
        map.insert("description".into(), serde_json::Value::String(v.into()));
    }
    if map.is_empty() {
        None
    } else {
        Some(serde_json::Value::Object(map))
    }
}

pub fn schema(app: Option<Arc<BtcMapClient>>) -> Schema<Query, Mutation, EmptySubscription> {
    let builder = Schema::build(Query, Mutation, EmptySubscription);
    if let Some(btcmap) = app {
        builder.data(btcmap).finish()
    } else {
        builder.finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_graphql::Request;
    use serde_json::json;
    use wiremock::{matchers::method, Mock, MockServer, ResponseTemplate};

    fn make_schema(server_url: &str) -> Schema<Query, Mutation, EmptySubscription> {
        let btcmap = Arc::new(BtcMapClient::new(
            server_url.to_string(),
            "api-key".to_string(),
            "blink".to_string(),
        ));
        schema(Some(btcmap))
    }

    fn authed_req(query: &str) -> Request {
        Request::new(query).data(AuthSubject { id: "user-1".to_string() })
    }

    fn ok_place_response() -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_json(json!({
            "jsonrpc": "2.0",
            "result": { "id": 1, "origin": "blink", "external_id": "blink:test-uuid" },
            "id": 1
        }))
    }

    fn ok_null_response() -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_json(json!({"jsonrpc":"2.0","result":null,"id":1}))
    }

    fn err_btcmap_response() -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_json(json!({
            "jsonrpc": "2.0",
            "error": { "code": -32600, "message": "Permission denied" },
            "id": 1
        }))
    }

    // ── validate_submit_place ───────────────────────────────────────────────

    fn valid_input() -> BtcMapSubmitPlaceInput {
        BtcMapSubmitPlaceInput {
            lat: 52.0,
            lon: 21.0,
            category: "atm".to_string(),
            name: "Test".to_string(),
            website: None,
            opening_hours: None,
            phone: None,
            description: None,
        }
    }

    #[test]
    fn validate_valid_input() {
        assert!(validate_submit_place(&valid_input()).is_ok());
    }

    #[test]
    fn validate_lat_too_low() {
        let mut i = valid_input(); i.lat = -91.0;
        assert!(validate_submit_place(&i).is_err());
    }

    #[test]
    fn validate_lat_too_high() {
        let mut i = valid_input(); i.lat = 91.0;
        assert!(validate_submit_place(&i).is_err());
    }

    #[test]
    fn validate_lat_boundary_ok() {
        let mut i = valid_input(); i.lat = -90.0;
        assert!(validate_submit_place(&i).is_ok());
        i.lat = 90.0;
        assert!(validate_submit_place(&i).is_ok());
    }

    #[test]
    fn validate_lon_too_low() {
        let mut i = valid_input(); i.lon = -181.0;
        assert!(validate_submit_place(&i).is_err());
    }

    #[test]
    fn validate_lon_too_high() {
        let mut i = valid_input(); i.lon = 181.0;
        assert!(validate_submit_place(&i).is_err());
    }

    #[test]
    fn validate_lon_boundary_ok() {
        let mut i = valid_input(); i.lon = -180.0;
        assert!(validate_submit_place(&i).is_ok());
        i.lon = 180.0;
        assert!(validate_submit_place(&i).is_ok());
    }

    #[test]
    fn validate_empty_name() {
        let mut i = valid_input(); i.name = "  ".to_string();
        assert!(validate_submit_place(&i).is_err());
    }

    #[test]
    fn validate_empty_category() {
        let mut i = valid_input(); i.category = String::new();
        assert!(validate_submit_place(&i).is_err());
    }

    #[test]
    fn validate_category_with_space() {
        let mut i = valid_input(); i.category = "fast food".to_string();
        assert!(validate_submit_place(&i).is_err());
    }

    // ── build_extra_fields ──────────────────────────────────────────────────

    #[test]
    fn extra_fields_all_none_returns_none() {
        assert!(build_extra_fields(None, None, None, None).is_none());
    }

    #[test]
    fn extra_fields_all_some() {
        let v = build_extra_fields(
            Some("https://ex.com"),
            Some("Mo-Fr 09:00-18:00"),
            Some("+48123"),
            Some("desc"),
        )
        .unwrap();
        let obj = v.as_object().unwrap();
        assert_eq!(obj["website"], "https://ex.com");
        assert_eq!(obj["opening_hours"], "Mo-Fr 09:00-18:00");
        assert_eq!(obj["phone"], "+48123");
        assert_eq!(obj["description"], "desc");
    }

    #[test]
    fn extra_fields_website_only() {
        let v = build_extra_fields(Some("https://ex.com"), None, None, None).unwrap();
        let obj = v.as_object().unwrap();
        assert!(obj.contains_key("website"));
        assert!(!obj.contains_key("phone"));
    }

    #[test]
    fn extra_fields_phone_only() {
        let v = build_extra_fields(None, None, Some("+1"), None).unwrap();
        assert!(v.as_object().unwrap().contains_key("phone"));
    }

    #[test]
    fn extra_fields_description_only() {
        let v = build_extra_fields(None, None, None, Some("cool")).unwrap();
        assert!(v.as_object().unwrap().contains_key("description"));
    }

    #[test]
    fn extra_fields_opening_hours_only() {
        let v = build_extra_fields(None, Some("24/7"), None, None).unwrap();
        assert!(v.as_object().unwrap().contains_key("opening_hours"));
    }

    // ── schema() ───────────────────────────────────────────────────────────

    #[test]
    fn schema_none_builds_ok() {
        let s = schema(None);
        assert!(!s.sdl().is_empty());
    }

    #[test]
    fn schema_some_builds_ok() {
        let btcmap = Arc::new(BtcMapClient::new(
            "http://localhost".into(),
            "key".into(),
            "blink".into(),
        ));
        let s = schema(Some(btcmap));
        assert!(!s.sdl().is_empty());
    }

    // ── mutations ──────────────────────────────────────────────────────────

    #[tokio::test]
    async fn submit_place_success() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ok_place_response())
            .mount(&server)
            .await;

        let s = make_schema(&server.uri());
        let resp = s
            .execute(authed_req(
                r#"mutation {
                  btcmapSubmitPlace(input: {
                    lat: 52.2, lon: 21.0, category: "atm", name: "My ATM"
                  }) { place { id origin externalId } }
                }"#,
            ))
            .await;
        assert!(resp.errors.is_empty(), "{:?}", resp.errors);
        let data = resp.data.into_json().unwrap();
        assert_eq!(data["btcmapSubmitPlace"]["place"]["id"], 1);
    }

    #[tokio::test]
    async fn submit_place_with_optional_fields() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ok_place_response())
            .mount(&server)
            .await;

        let s = make_schema(&server.uri());
        let resp = s
            .execute(authed_req(
                r#"mutation {
                  btcmapSubmitPlace(input: {
                    lat: 52.2, lon: 21.0, category: "restaurant", name: "R",
                    website: "https://r.com", openingHours: "Mo-Fr", phone: "+1", description: "d"
                  }) { place { id } }
                }"#,
            ))
            .await;
        assert!(resp.errors.is_empty(), "{:?}", resp.errors);
    }

    #[tokio::test]
    async fn submit_place_no_auth() {
        let s = schema(None);
        let resp = s
            .execute(Request::new(
                r#"mutation { btcmapSubmitPlace(input:{lat:0,lon:0,category:"atm",name:"X"}) { place { id } } }"#,
            ))
            .await;
        assert!(!resp.errors.is_empty());
    }

    #[tokio::test]
    async fn submit_place_validation_error_lat() {
        let s = schema(None);
        let resp = s
            .execute(authed_req(
                r#"mutation { btcmapSubmitPlace(input:{lat:999,lon:0,category:"atm",name:"X"}) { place { id } } }"#,
            ))
            .await;
        assert!(!resp.errors.is_empty());
        assert!(resp.errors[0].message.contains("Latitude"));
    }

    #[tokio::test]
    async fn submit_place_validation_error_lon() {
        let s = schema(None);
        let resp = s
            .execute(authed_req(
                r#"mutation { btcmapSubmitPlace(input:{lat:0,lon:999,category:"atm",name:"X"}) { place { id } } }"#,
            ))
            .await;
        assert!(!resp.errors.is_empty());
        assert!(resp.errors[0].message.contains("Longitude"));
    }

    #[tokio::test]
    async fn submit_place_validation_error_name() {
        let s = schema(None);
        let resp = s
            .execute(authed_req(
                r#"mutation { btcmapSubmitPlace(input:{lat:0,lon:0,category:"atm",name:"  "}) { place { id } } }"#,
            ))
            .await;
        assert!(!resp.errors.is_empty());
    }

    #[tokio::test]
    async fn submit_place_validation_error_category() {
        let s = schema(None);
        let resp = s
            .execute(authed_req(
                r#"mutation { btcmapSubmitPlace(input:{lat:0,lon:0,category:"fast food",name:"X"}) { place { id } } }"#,
            ))
            .await;
        assert!(!resp.errors.is_empty());
    }

    #[tokio::test]
    async fn submit_place_btcmap_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(err_btcmap_response())
            .mount(&server)
            .await;

        let s = make_schema(&server.uri());
        let resp = s
            .execute(authed_req(
                r#"mutation { btcmapSubmitPlace(input:{lat:0,lon:0,category:"atm",name:"X"}) { place { id } } }"#,
            ))
            .await;
        assert!(!resp.errors.is_empty());
        assert!(resp.errors[0].message.contains("Failed to submit place"));
    }

    #[tokio::test]
    async fn verify_element_success() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ok_null_response())
            .mount(&server)
            .await;

        let s = make_schema(&server.uri());
        let resp = s
            .execute(authed_req(
                r#"mutation { btcmapVerifyElement(input:{elementId:"node:1"}) { success } }"#,
            ))
            .await;
        assert!(resp.errors.is_empty(), "{:?}", resp.errors);
        let data = resp.data.into_json().unwrap();
        assert_eq!(data["btcmapVerifyElement"]["success"], true);
    }

    #[tokio::test]
    async fn verify_element_no_auth() {
        let s = schema(None);
        let resp = s
            .execute(Request::new(
                r#"mutation { btcmapVerifyElement(input:{elementId:"node:1"}) { success } }"#,
            ))
            .await;
        assert!(!resp.errors.is_empty());
    }

    #[tokio::test]
    async fn verify_element_btcmap_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(err_btcmap_response())
            .mount(&server)
            .await;

        let s = make_schema(&server.uri());
        let resp = s
            .execute(authed_req(
                r#"mutation { btcmapVerifyElement(input:{elementId:"node:1"}) { success } }"#,
            ))
            .await;
        assert!(!resp.errors.is_empty());
        assert!(resp.errors[0].message.contains("Failed to verify element"));
    }

    #[tokio::test]
    async fn query_me_entity() {
        let s = schema(None);
        let resp = s
            .execute(Request::new(r#"{ _entities(representations:[{__typename:"User",id:"u1"}]) { ... on User { id } } }"#))
            .await;
        assert!(resp.errors.is_empty(), "{:?}", resp.errors);
    }
}
