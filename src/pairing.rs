use crate::{db, runtime::Shared};
use axum::{Json, extract::State, http::HeaderMap};
use serde_json::{Value, json};
use std::{collections::HashMap, sync::Mutex};

#[derive(Default)]
pub struct Pairing {
    pending: Mutex<HashMap<String, i64>>,
}
impl Pairing {
    fn issue(&self, now: i64) -> anyhow::Result<(String, i64)> {
        let mut pending = self.pending.lock().unwrap();
        pending.retain(|_, expiry| *expiry > now);
        anyhow::ensure!(
            pending.len() < 8,
            "Eight device links are already pending. Wait for them to expire."
        );
        let code = format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
        let expires = now + 600;
        pending.insert(code.clone(), expires);
        Ok((code, expires))
    }
    fn consume(&self, code: &str, now: i64) -> bool {
        let mut pending = self.pending.lock().unwrap();
        pending.retain(|_, expiry| *expiry > now);
        if code.len() != 64 {
            return false;
        }
        let found = pending
            .keys()
            .find(|key| crate::web::valid_token(key, code))
            .cloned();
        found.is_some_and(|key| pending.remove(&key).is_some())
    }
}
pub async fn issue(State(app): State<Shared>) -> Result<Json<Value>, crate::web::Error> {
    let (code, expires) = app.pairing.issue(db::now())?;
    Ok(Json(
        json!({"url":format!("{}/#pair={code}",app.config.public_url.trim_end_matches('/')),"expires_at":expires}),
    ))
}
pub async fn claim(
    State(app): State<Shared>,
    headers: HeaderMap,
    Json(v): Json<Value>,
) -> Result<Json<Value>, crate::web::Error> {
    if !headers
        .get("origin")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| app.config.allows_origin(v))
    {
        return Err(
            anyhow::anyhow!("Open the device link in your browser on this Kindred server").into(),
        );
    }
    if !app
        .pairing
        .consume(v["code"].as_str().unwrap_or(""), db::now())
    {
        return Err(anyhow::anyhow!("This device link is invalid, expired or already used. Create a fresh link from a connected device.").into());
    }
    Ok(Json(json!({"token":app.token})))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn links_are_expiring_bounded_and_single_use() {
        let state = Pairing::default();
        let (code, expiry) = state.issue(10).unwrap();
        assert_eq!(expiry, 610);
        assert!(!state.consume("wrong", 11));
        assert!(state.consume(&code, 11));
        assert!(!state.consume(&code, 11));
        let (code, _) = state.issue(10).unwrap();
        assert!(!state.consume(&code, 610));
        for _ in 0..8 {
            state.issue(1000).unwrap();
        }
        assert!(state.issue(1000).is_err());
        assert!(state.issue(1600).is_ok());
    }
    #[tokio::test]
    async fn linking_requires_owner_authorization_and_same_origin_claim() {
        let app = crate::tests::app();
        let router = crate::web::router(app.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async { axum::serve(listener, router).await.unwrap() });
        let client = reqwest::Client::new();
        let r = client
            .post(format!("{base}/api/devices/link"))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 401);
        let (code, _) = app.pairing.issue(db::now()).unwrap();
        for origin in ["https://other.example", ""] {
            let r = client
                .post(format!("{base}/device/claim"))
                .header("origin", origin)
                .json(&json!({"code":code}))
                .send()
                .await
                .unwrap();
            assert_eq!(r.status(), 400);
        }
        let req = || {
            client
                .post(format!("{base}/device/claim"))
                .header("origin", app.config.public_url.clone())
                .json(&json!({"code":code}))
        };
        let r = req().send().await.unwrap();
        assert_eq!(r.status(), 200);
        assert_eq!(r.headers()["cache-control"], "no-store");
        assert_eq!(r.json::<Value>().await.unwrap()["token"], app.token);
        assert_eq!(req().send().await.unwrap().status(), 400);
        server.abort();
    }
}
