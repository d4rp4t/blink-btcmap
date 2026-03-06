use anyhow::Result;
use serde::Deserialize;
use serde_json::{json, Value};

pub struct BtcMapClient {
    client: reqwest::Client,
    api_url: String,
    api_key: String,
    origin: String,
}

#[derive(Debug, Deserialize)]
pub struct SubmitPlaceResponse {
    pub id: i64,
    pub origin: String,
    pub external_id: String,
}

impl BtcMapClient {
    pub fn new(api_url: String, api_key: String, origin: String) -> Self {
        Self {
            client: reqwest::Client::new(),
            api_url,
            api_key,
            origin,
        }
    }

    async fn call(&self, method: &str, params: Value) -> Result<Value> {
        let body = json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
            "id": 1
        });

        let resp = self
            .client
            .post(&self.api_url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .json(&body)
            .send()
            .await?
            .json::<Value>()
            .await?;

        if let Some(error) = resp.get("error") {
            anyhow::bail!("BtcMap error: {error}");
        }

        Ok(resp["result"].clone())
    }

    #[tracing::instrument(name = "btcmap.submit_place", skip(self, extra_fields), err)]
    pub async fn submit_place(
        &self,
        external_id: &str,
        lat: f64,
        lon: f64,
        category: &str,
        name: &str,
        extra_fields: Option<Value>,
    ) -> Result<SubmitPlaceResponse> {
        let mut params = json!({
            "origin": self.origin,
            "external_id": external_id,
            "lat": lat,
            "lon": lon,
            "category": category,
            "name": name,
        });

        if let Some(extra) = extra_fields {
            params["extra_fields"] = extra;
        }

        let result = self.call("submit_place", params).await?;
        Ok(serde_json::from_value(result)?)
    }

    #[tracing::instrument(name = "btcmap.verify_element", skip(self), err)]
    pub async fn verify_element(&self, element_id: &str) -> Result<()> {
        let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
        for tag in &["survey:date", "check_date", "check_date:currency:XBT"] {
            let params = json!({
                "id": element_id,
                "tag": tag,
                "value": today,
            });
            self.call("set_element_tag", params).await.map_err(|e| {
                anyhow::anyhow!("Failed to set tag '{tag}' on element '{element_id}': {e}")
            })?;
        }
        Ok(())
    }
}
