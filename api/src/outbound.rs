use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::Duration;

use anyhow::{anyhow, bail, Context};
use reqwest::header::LOCATION;
use url::Url;

const MAX_GET_ATTEMPTS: usize = 3;
const GET_RETRY_DELAYS: [Duration; MAX_GET_ATTEMPTS - 1] =
    [Duration::from_millis(500), Duration::from_millis(1500)];
const MAX_GET_REDIRECTS: usize = 3;

#[derive(Debug)]
pub struct GetBytesResponse {
    pub status: reqwest::StatusCode,
    pub bytes: Vec<u8>,
    /// 包含首次请求在内的实际请求轮次。
    pub attempts: usize,
}

#[derive(Debug, Clone, Copy)]
enum GetFailureKind {
    Validation,
    Dns,
    Client,
    Connect,
    Timeout,
    ResponseBody,
    Request,
    ResponseTooLarge,
    Redirect,
}

impl GetFailureKind {
    const fn retryable(self) -> bool {
        matches!(
            self,
            Self::Dns | Self::Connect | Self::Timeout | Self::ResponseBody
        )
    }
}

impl fmt::Display for GetFailureKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match self {
            Self::Validation => "validation",
            Self::Dns => "dns",
            Self::Client => "client",
            Self::Connect => "connect",
            Self::Timeout => "timeout",
            Self::ResponseBody => "response body",
            Self::Request => "request",
            Self::ResponseTooLarge => "response too large",
            Self::Redirect => "redirect",
        };
        f.write_str(label)
    }
}

#[derive(Debug)]
struct GetFailure {
    kind: GetFailureKind,
    source: anyhow::Error,
}

impl GetFailure {
    fn new(kind: GetFailureKind, source: impl Into<anyhow::Error>) -> Self {
        Self {
            kind,
            source: source.into(),
        }
    }
}

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

fn validate_url(raw: &str) -> anyhow::Result<(Url, String)> {
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
    Ok((url, host))
}

async fn resolve_public_host(host: &str) -> anyhow::Result<Vec<SocketAddr>> {
    let addrs = resolve_host(host).await?;
    ensure_public_addresses(&addrs)?;
    Ok(addrs)
}

async fn resolve_host(host: &str) -> anyhow::Result<Vec<SocketAddr>> {
    Ok(tokio::net::lookup_host((host, 443))
        .await
        .context("DNS resolution failed")?
        .collect())
}

fn ensure_public_addresses(addrs: &[SocketAddr]) -> anyhow::Result<()> {
    if addrs.is_empty() || addrs.iter().any(|v| !is_public_ip(v.ip())) {
        bail!("outbound URL resolves to a forbidden address");
    }
    Ok(())
}

async fn validate_and_resolve(raw: &str) -> anyhow::Result<(Url, String, Vec<SocketAddr>)> {
    let (url, host) = validate_url(raw)?;
    let addrs = resolve_public_host(&host).await?;
    Ok((url, host, addrs))
}

pub async fn validate_public_https(raw: &str) -> anyhow::Result<()> {
    validate_and_resolve(raw).await.map(|_| ())
}

fn build_client(
    host: &str,
    addrs: &[SocketAddr],
    timeout: Duration,
) -> anyhow::Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .timeout(timeout)
        .connect_timeout(Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::none())
        .resolve_to_addrs(host, addrs)
        .user_agent("SaltedBlog/1.0")
        .build()?)
}

async fn client_for(raw: &str, timeout: Duration) -> anyhow::Result<(Url, reqwest::Client)> {
    let (url, host, addrs) = validate_and_resolve(raw).await?;
    let client = build_client(&host, &addrs, timeout)?;
    Ok((url, client))
}

fn retryable_status(status: reqwest::StatusCode) -> bool {
    status.as_u16() == 408 || status.as_u16() == 429 || status.is_server_error()
}

fn request_failure_kind(error: &reqwest::Error) -> GetFailureKind {
    if error.is_timeout() {
        GetFailureKind::Timeout
    } else if error.is_connect() {
        GetFailureKind::Connect
    } else if error.is_body() {
        GetFailureKind::ResponseBody
    } else {
        GetFailureKind::Request
    }
}

fn can_follow_redirect(redirects_followed: usize) -> bool {
    redirects_followed < MAX_GET_REDIRECTS
}

async fn get_bytes_once(
    raw: &str,
    max_bytes: usize,
    timeout: Duration,
) -> Result<GetBytesResponse, GetFailure> {
    let mut current = raw.to_string();
    let mut redirects_followed = 0usize;

    loop {
        let (url, host) =
            validate_url(&current).map_err(|e| GetFailure::new(GetFailureKind::Validation, e))?;
        let addrs = resolve_host(&host)
            .await
            .map_err(|e| GetFailure::new(GetFailureKind::Dns, e))?;
        ensure_public_addresses(&addrs)
            .map_err(|e| GetFailure::new(GetFailureKind::Validation, e))?;
        let client = build_client(&host, &addrs, timeout)
            .map_err(|e| GetFailure::new(GetFailureKind::Client, e))?;
        let mut response = client
            .get(url.clone())
            .send()
            .await
            .map_err(|e| GetFailure::new(request_failure_kind(&e), e))?;

        if response.status().is_redirection() {
            if !can_follow_redirect(redirects_followed) {
                return Err(GetFailure::new(
                    GetFailureKind::Redirect,
                    anyhow!("too many redirects"),
                ));
            }
            let location = response
                .headers()
                .get(LOCATION)
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| anyhow!("redirect missing location"))
                .map_err(|e| GetFailure::new(GetFailureKind::Redirect, e))?;
            current = url
                .join(location)
                .map_err(|e| GetFailure::new(GetFailureKind::Redirect, e))?
                .to_string();
            redirects_followed += 1;
            continue;
        }

        if response
            .content_length()
            .is_some_and(|n| n > max_bytes as u64)
        {
            return Err(GetFailure::new(
                GetFailureKind::ResponseTooLarge,
                anyhow!("response exceeds size limit"),
            ));
        }
        let status = response.status();
        let mut output = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|e| GetFailure::new(request_failure_kind(&e), e))?
        {
            if output.len().saturating_add(chunk.len()) > max_bytes {
                return Err(GetFailure::new(
                    GetFailureKind::ResponseTooLarge,
                    anyhow!("response exceeds size limit"),
                ));
            }
            output.extend_from_slice(&chunk);
        }
        return Ok(GetBytesResponse {
            status,
            bytes: output,
            attempts: 0,
        });
    }
}

pub async fn get_bytes(
    raw: &str,
    max_bytes: usize,
    timeout: Duration,
) -> anyhow::Result<GetBytesResponse> {
    for attempt in 1..=MAX_GET_ATTEMPTS {
        match get_bytes_once(raw, max_bytes, timeout).await {
            Ok(response) if retryable_status(response.status) && attempt < MAX_GET_ATTEMPTS => {
                tracing::warn!(
                    attempt,
                    status = response.status.as_u16(),
                    "retrying outbound GET after retryable HTTP status"
                );
                tokio::time::sleep(GET_RETRY_DELAYS[attempt - 1]).await;
            }
            Ok(mut response) => {
                response.attempts = attempt;
                return Ok(response);
            }
            Err(failure) if failure.kind.retryable() && attempt < MAX_GET_ATTEMPTS => {
                tracing::warn!(
                    attempt,
                    kind = %failure.kind,
                    error = ?failure.source,
                    "retrying outbound GET after transient failure"
                );
                tokio::time::sleep(GET_RETRY_DELAYS[attempt - 1]).await;
            }
            Err(failure) => {
                tracing::warn!(
                    attempt,
                    kind = %failure.kind,
                    error = ?failure.source,
                    "outbound GET failed"
                );
                return Err(anyhow!(
                    "GET request failed after {attempt} attempt(s) ({})",
                    failure.kind
                ));
            }
        }
    }
    unreachable!("GET retry loop always returns")
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

    #[test]
    fn retryable_statuses_are_limited_to_transient_responses() {
        assert!(retryable_status(reqwest::StatusCode::REQUEST_TIMEOUT));
        assert!(retryable_status(reqwest::StatusCode::TOO_MANY_REQUESTS));
        assert!(retryable_status(reqwest::StatusCode::BAD_GATEWAY));
        assert!(!retryable_status(reqwest::StatusCode::OK));
        assert!(!retryable_status(reqwest::StatusCode::NOT_FOUND));
    }

    #[test]
    fn redirect_limit_allows_exactly_three_hops() {
        assert!(can_follow_redirect(0));
        assert!(can_follow_redirect(2));
        assert!(!can_follow_redirect(3));
    }

    #[test]
    fn only_transient_get_failures_are_retryable() {
        assert!(GetFailureKind::Dns.retryable());
        assert!(GetFailureKind::Connect.retryable());
        assert!(GetFailureKind::Timeout.retryable());
        assert!(GetFailureKind::ResponseBody.retryable());
        assert!(!GetFailureKind::Request.retryable());
        assert!(!GetFailureKind::Validation.retryable());
        assert!(!GetFailureKind::ResponseTooLarge.retryable());
        assert!(!GetFailureKind::Redirect.retryable());
    }
}
