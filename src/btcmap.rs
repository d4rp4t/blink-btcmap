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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wiremock::{matchers::method, Mock, MockServer, ResponseTemplate};

    fn make_client(url: &str) -> BtcMapClient {
        BtcMapClient::new(url.to_string(), "test-key".to_string(), "blink".to_string())
    }

    fn ok_response(result: serde_json::Value) -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_json(json!({
            "jsonrpc": "2.0",
            "result": result,
            "id": 1
        }))
    }

    fn err_response(msg: &str) -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_json(json!({
            "jsonrpc": "2.0",
            "error": { "code": -32600, "message": msg },
            "id": 1
        }))
    }

    #[tokio::test]
    async fn new_sets_fields() {
        let c = make_client("http://localhost");
        assert_eq!(c.api_url, "http://localhost");
        assert_eq!(c.api_key, "test-key");
        assert_eq!(c.origin, "blink");
    }

    #[tokio::test]
    async fn submit_place_success_without_extra_fields() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ok_response(json!({"id": 42, "origin": "blink", "external_id": "b:u"})))
            .mount(&server)
            .await;

        let result = make_client(&server.uri())
            .submit_place("b:u", 51.5, -0.1, "restaurant", "Cafe", None)
            .await
            .unwrap();
        assert_eq!(result.id, 42);
        assert_eq!(result.origin, "blink");
        assert_eq!(result.external_id, "b:u");
    }

    #[tokio::test]
    async fn submit_place_success_with_extra_fields() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ok_response(json!({"id": 7, "origin": "blink", "external_id": "b:e"})))
            .mount(&server)
            .await;

        let extra = json!({"website": "https://example.com"});
        let result = make_client(&server.uri())
            .submit_place("b:e", 48.8, 2.3, "atm", "My ATM", Some(extra))
            .await
            .unwrap();
        assert_eq!(result.id, 7);
    }

    #[tokio::test]
    async fn submit_place_btcmap_error_response() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(err_response("Forbidden"))
            .mount(&server)
            .await;

        let err = make_client(&server.uri())
            .submit_place("x", 0.0, 0.0, "atm", "X", None)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("BtcMap error"));
    }

    #[tokio::test]
    async fn submit_place_network_error() {
        let err = make_client("http://127.0.0.1:1")
            .submit_place("x", 0.0, 0.0, "atm", "X", None)
            .await
            .unwrap_err();
        assert!(err.to_string().len() > 0);
    }

    #[tokio::test]
    async fn verify_element_success() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ok_response(json!(null)))
            .mount(&server)
            .await;

        make_client(&server.uri())
            .verify_element("node:1")
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn verify_element_error_on_first_tag() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(err_response("Forbidden"))
            .mount(&server)
            .await;

        let err = make_client(&server.uri())
            .verify_element("node:1")
            .await
            .unwrap_err();
        assert!(err.to_string().contains("survey:date"));
    }

    #[tokio::test]
    async fn verify_element_error_on_second_tag() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ok_response(json!(null)))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .respond_with(err_response("Forbidden"))
            .mount(&server)
            .await;

        let err = make_client(&server.uri())
            .verify_element("node:2")
            .await
            .unwrap_err();
        assert!(err.to_string().contains("check_date"));
    }

    #[tokio::test]
    async fn verify_element_error_on_third_tag() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ok_response(json!(null)))
            .up_to_n_times(2)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .respond_with(err_response("Forbidden"))
            .mount(&server)
            .await;

        let err = make_client(&server.uri())
            .verify_element("node:3")
            .await
            .unwrap_err();
        assert!(err.to_string().contains("check_date:currency:XBT"));
    }
}
