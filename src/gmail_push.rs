//! Authenticate Google Pub/Sub delivery using Google's public signing keys.
use anyhow::{Context, Result, ensure};
use axum::http::{HeaderMap, header};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::Value;
use std::{sync::OnceLock, time::Duration};

fn verify_token(token: &str, audience: &str, email: &str, keys: &Value, now: i64) -> Result<()> {
    ensure!(token.len() <= 16000, "Invalid push identity token");
    let parts: Vec<_> = token.split('.').collect();
    ensure!(parts.len() == 3, "Invalid push identity token");
    let head: Value = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[0])?)?;
    ensure!(
        head["alg"] == "RS256",
        "Unexpected Google signature algorithm"
    );
    let kid = head["kid"]
        .as_str()
        .filter(|s| !s.is_empty())
        .context("Missing Google key ID")?;
    let key = keys["keys"]
        .as_array()
        .and_then(|a| {
            a.iter()
                .find(|v| v["kid"] == kid && v["kty"] == "RSA" && v["alg"] == "RS256")
        })
        .context("Unknown Google signing key")?;
    let n = URL_SAFE_NO_PAD.decode(key["n"].as_str().context("Missing RSA modulus")?)?;
    let e = URL_SAFE_NO_PAD.decode(key["e"].as_str().context("Missing RSA exponent")?)?;
    let signature = URL_SAFE_NO_PAD.decode(parts[2])?;
    ring::signature::RsaPublicKeyComponents { n, e }
        .verify(
            &ring::signature::RSA_PKCS1_2048_8192_SHA256,
            format!("{}.{}", parts[0], parts[1]).as_bytes(),
            &signature,
        )
        .map_err(|_| anyhow::anyhow!("Invalid Google push signature"))?;
    let claims: Value = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[1])?)?;
    ensure!(
        matches!(
            claims["iss"].as_str(),
            Some("https://accounts.google.com" | "accounts.google.com")
        ),
        "Unexpected push identity issuer"
    );
    ensure!(
        claims["aud"] == audience && claims["email"] == email && claims["email_verified"] == true,
        "Push identity does not match this monitor"
    );
    ensure!(
        claims["exp"].as_i64().is_some_and(|v| v > now)
            && claims["iat"]
                .as_i64()
                .is_some_and(|v| v <= now + 60 && v >= now - 7200),
        "Expired or invalid push identity token"
    );
    Ok(())
}
pub async fn verify(headers: &HeaderMap, audience: &str, email: &str) -> Result<()> {
    let token = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .context("Google push authentication is required")?;
    ensure!(
        token.len() <= 16000 && token.split('.').count() == 3,
        "Invalid push identity token"
    );
    static CACHE: OnceLock<tokio::sync::Mutex<(i64, Value)>> = OnceLock::new();
    let mut cache = CACHE
        .get_or_init(|| tokio::sync::Mutex::new((0, Value::Null)))
        .lock()
        .await;
    let now = crate::db::now();
    if cache.0 <= now {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        let mut res = http
            .get("https://www.googleapis.com/oauth2/v3/certs")
            .send()
            .await?
            .error_for_status()?;
        let mut body = Vec::new();
        while let Some(chunk) = res.chunk().await? {
            ensure!(
                body.len() + chunk.len() <= 65536,
                "Google signing keys response is too large"
            );
            body.extend_from_slice(&chunk);
        }
        let keys: Value = serde_json::from_slice(&body)?;
        ensure!(
            keys["keys"].as_array().is_some_and(|a| !a.is_empty()),
            "Google signing keys are unavailable"
        );
        *cache = (now + 300, keys);
    }
    verify_token(token, audience, email, &cache.1, now)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn signed_google_identity_requires_exact_audience_service_account_and_time() {
        let fixture: Value =
            serde_json::from_str(include_str!("test-fixtures/google-push-tokens.json")).unwrap();
        let check = |token: &str| {
            verify_token(
                token,
                "https://kindred.example.test/hooks/gmail/test",
                "push@my-project.iam.gserviceaccount.com",
                &fixture["keys"],
                1700000000,
            )
        };
        assert!(check(fixture["valid"].as_str().unwrap()).is_ok());
        for key in [
            "wrong_audience",
            "wrong_email",
            "unverified_email",
            "expired",
            "future",
            "wrong_issuer",
        ] {
            assert!(check(fixture[key].as_str().unwrap()).is_err(), "{key}");
        }
        let mut tampered = fixture["valid"].as_str().unwrap().as_bytes().to_vec();
        tampered[50] = if tampered[50] == b'a' { b'b' } else { b'a' };
        assert!(check(std::str::from_utf8(&tampered).unwrap()).is_err());
    }
    #[test]
    fn rejects_unsigned_wrong_algorithm_and_malformed_tokens() {
        for token in [
            "",
            "a.b",
            "eyJhbGciOiJub25lIn0.e30.",
            "eyJhbGciOiJIUzI1NiJ9.e30.YQ",
        ] {
            assert!(
                verify_token(
                    token,
                    "https://example.com/hooks/gmail/test",
                    "test@example.iam.gserviceaccount.com",
                    &serde_json::json!({"keys":[]}),
                    100
                )
                .is_err()
            );
        }
    }
}
