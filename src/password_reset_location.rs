use serde_json::Value;
use std::net::IpAddr;

pub(super) fn public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v) => {
            let [a, b, _, _] = v.octets();
            !v.is_private()
                && !v.is_loopback()
                && !v.is_link_local()
                && !v.is_documentation()
                && a > 0
                && a < 224
                && !(a == 100 && (64..=127).contains(&b))
                && !(a == 198 && (18..=19).contains(&b))
                && !(a == 192 && b == 0)
        }
        IpAddr::V6(v) => v
            .to_ipv4_mapped()
            .map(|v| public_ip(v.into()))
            .unwrap_or_else(|| {
                let s = v.segments();
                s[0] & 0xe000 == 0x2000
                    && !(s[0] == 0x2001 && s[1] == 0xdb8)
                    && s[0] != 0x2002
                    && !(s[0] == 0x2001 && s[1] < 0x200)
            }),
    }
}
fn description(v: &Value) -> Option<String> {
    if v["success"] != true {
        return None;
    }
    let field = |name: &str| {
        v[name]
            .as_str()
            .filter(|s| !s.trim().is_empty() && s.len() <= 100 && !s.chars().any(char::is_control))
    };
    let country = field("country")?;
    let mut parts = Vec::new();
    if let Some(city) = field("city") {
        parts.push(city);
    }
    if let Some(region) = field("region") {
        if !parts.contains(&region) {
            parts.push(region);
        }
    }
    if !parts.contains(&country) {
        parts.push(country);
    }
    Some(parts.join(", "))
}
pub(super) async fn lookup(ip: IpAddr) -> Option<String> {
    if !public_ip(ip) {
        return None;
    }
    static SLOTS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(4);
    let _slot = SLOTS.try_acquire().ok()?;
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(3))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .ok()?;
    // Only the observed public IP is sent: no username, request secret or ID.
    let mut response = client
        .get(format!(
            "https://ipwho.is/{ip}?fields=success,city,region,country"
        ))
        .send()
        .await
        .ok()?
        .error_for_status()
        .ok()?;
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.ok()? {
        if bytes.len() + chunk.len() > 16384 {
            return None;
        }
        bytes.extend_from_slice(&chunk);
    }
    description(&serde_json::from_slice::<Value>(&bytes).ok()?)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn private_and_special_addresses_are_never_sent() {
        for ip in [
            "127.0.0.1",
            "10.0.0.1",
            "172.16.2.3",
            "192.168.1.2",
            "100.64.0.1",
            "169.254.1.1",
            "192.0.2.10",
            "198.51.100.1",
            "203.0.113.9",
            "224.0.0.1",
            "240.0.0.1",
            "0.0.0.0",
            "198.18.0.1",
            "::1",
            "fc00::1",
            "fe80::1",
            "2001:db8::1",
            "::ffff:127.0.0.1",
        ] {
            assert!(!public_ip(ip.parse().unwrap()), "{ip}");
        }
        assert!(public_ip("8.8.8.8".parse().unwrap()));
        assert!(public_ip("2606:4700:4700::1111".parse().unwrap()));
    }
    #[test]
    fn location_is_optional_and_bounded() {
        assert_eq!(
            description(
                &serde_json::json!({"success":true,"city":"Boston","region":"Massachusetts","country":"United States"})
            ),
            Some("Boston, Massachusetts, United States".into())
        );
        assert_eq!(
            description(&serde_json::json!({"success":false,"message":"Rate limit exceeded"})),
            None
        );
        assert_eq!(
            description(&serde_json::json!({"success":true,"country":""})),
            None
        );
    }
}
