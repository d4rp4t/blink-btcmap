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
