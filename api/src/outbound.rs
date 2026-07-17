use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::Duration;

use anyhow::{anyhow, bail, Context};
use reqwest::header::LOCATION;
use url::Url;

fn public_v4(ip: Ipv4Addr) -> bool {
    let o = ip.octets();
    !(ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local()
        || ip.is_multicast()
        || ip.is_unspecified()
        || ip == Ipv4Addr::BROADCAST
        || o[0] == 0
        || o[0] >= 224
        || (o[0] == 100 && (64..=127).contains(&o[1]))
        || (o[0] == 192 && o[1] == 0 && o[2] == 0)
        || (o[0] == 192 && o[1] == 0 && o[2] == 2)
        || (o[0] == 198 && (o[1] == 18 || o[1] == 19))
        || (o[0] == 198 && o[1] == 51 && o[2] == 100)
        || (o[0] == 203 && o[1] == 0 && o[2] == 113))
}

fn public_v6(ip: Ipv6Addr) -> bool {
    let s = ip.segments();
    if let Some(v4) = ip.to_ipv4_mapped() {
        return public_v4(v4);
    }
    !(ip.is_loopback()
        || ip.is_unspecified()
        || ip.is_multicast()
        || (s[0] & 0xfe00) == 0xfc00
        || (s[0] & 0xffc0) == 0xfe80
        || (s[0] == 0x2001 && s[1] == 0x0db8))
}

pub fn is_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v) => public_v4(v),
        IpAddr::V6(v) => public_v6(v),
    }
}

async fn validate_and_resolve(raw: &str) -> anyhow::Result<(Url, String, Vec<SocketAddr>)> {
    let url = Url::parse(raw).context("invalid outbound URL")?;
    if url.scheme() != "https" {
        bail!("outbound URL must use https");
    }
    if !url.username().is_empty() || url.password().is_some() {
        bail!("URL credentials are forbidden");
    }
    if url.port_or_known_default() != Some(443) {
        bail!("only HTTPS port 443 is allowed");
    }
    let host = url
        .host_str()
        .ok_or_else(|| anyhow!("URL host required"))?
        .to_string();
    let addrs: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), 443))
        .await
        .context("DNS resolution failed")?
        .collect();
    if addrs.is_empty() || addrs.iter().any(|v| !is_public_ip(v.ip())) {
        bail!("outbound URL resolves to a forbidden address");
    }
    Ok((url, host, addrs))
}

pub async fn validate_public_https(raw: &str) -> anyhow::Result<()> {
    validate_and_resolve(raw).await.map(|_| ())
}

async fn client_for(raw: &str, timeout: Duration) -> anyhow::Result<(Url, reqwest::Client)> {
    let (url, host, addrs) = validate_and_resolve(raw).await?;
    let client = reqwest::Client::builder()
        .timeout(timeout)
        .connect_timeout(Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::none())
        .resolve_to_addrs(&host, &addrs)
        .user_agent("SaltedBlog/1.0")
        .build()?;
    Ok((url, client))
}

pub async fn get_bytes(
    raw: &str,
    max_bytes: usize,
    timeout: Duration,
) -> anyhow::Result<(reqwest::StatusCode, Vec<u8>)> {
    let mut current = raw.to_string();
    for _ in 0..=3 {
        let (url, client) = client_for(&current, timeout).await?;
        let mut response = client.get(url.clone()).send().await?;
        if response.status().is_redirection() {
            let location = response
                .headers()
                .get(LOCATION)
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| anyhow!("redirect missing location"))?;
            current = url.join(location)?.to_string();
            continue;
        }
        if response
            .content_length()
            .is_some_and(|n| n > max_bytes as u64)
        {
            bail!("response exceeds size limit");
        }
        let status = response.status();
        let mut output = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            if output.len().saturating_add(chunk.len()) > max_bytes {
                bail!("response exceeds size limit");
            }
            output.extend_from_slice(&chunk);
        }
        return Ok((status, output));
    }
    bail!("too many redirects")
}

pub async fn post_json(
    raw: &str,
    bearer: &str,
    body: &serde_json::Value,
    max_bytes: usize,
    timeout: Duration,
) -> anyhow::Result<(reqwest::StatusCode, String)> {
    let (url, client) = client_for(raw, timeout).await?;
    let mut response = client
        .post(url)
        .bearer_auth(bearer)
        .json(body)
        .send()
        .await?;
    if response.status().is_redirection() {
        bail!("LLM redirects are forbidden");
    }
    if response
        .content_length()
        .is_some_and(|n| n > max_bytes as u64)
    {
        bail!("LLM response exceeds size limit");
    }
    let status = response.status();
    let mut output = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if output.len().saturating_add(chunk.len()) > max_bytes {
            bail!("LLM response exceeds size limit");
        }
        output.extend_from_slice(&chunk);
    }
    Ok((
        status,
        String::from_utf8(output).context("LLM response is not UTF-8")?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_private_v4() {
        assert!(!is_public_ip("127.0.0.1".parse().unwrap()));
        assert!(!is_public_ip("169.254.169.254".parse().unwrap()));
        assert!(!is_public_ip("10.1.2.3".parse().unwrap()));
    }
    #[test]
    fn rejects_private_v6() {
        assert!(!is_public_ip("::1".parse().unwrap()));
        assert!(!is_public_ip("fc00::1".parse().unwrap()));
        assert!(!is_public_ip("fe80::1".parse().unwrap()));
    }
    #[test]
    fn accepts_public() {
        assert!(is_public_ip("1.1.1.1".parse().unwrap()));
        assert!(is_public_ip("2606:4700:4700::1111".parse().unwrap()));
    }
}
