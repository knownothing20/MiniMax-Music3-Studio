use std::{
    env, fmt,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, ToSocketAddrs},
    sync::OnceLock,
    time::Duration,
};

use axum::{
    extract::{Request, State},
    http::{Method, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use futures_util::StreamExt;

const SESSION_HEADER: &str = "x-studio-session";
const MAX_IMAGE_BYTES: usize = 20 * 1024 * 1024;
const MAX_REDIRECTS: usize = 3;
const ALLOWED_IMAGE_TYPES: &[&str] = &[
    "image/jpeg",
    "image/png",
    "image/webp",
    "image/gif",
    "image/avif",
];

static SESSION_TOKEN: OnceLock<SessionToken> = OnceLock::new();

#[derive(Clone)]
pub struct SessionToken(String);

impl SessionToken {
    pub fn global() -> Self {
        SESSION_TOKEN
            .get_or_init(|| {
                let configured = cfg!(debug_assertions)
                    .then(|| env::var("MINIMAX_STUDIO_SESSION_TOKEN").ok())
                    .flatten()
                    .filter(|value| value.trim().len() >= 32);
                let value = configured.unwrap_or_else(|| {
                    // UUIDv7 contributes 74 random bits from the OS-backed UUID
                    // RNG. Four independent values provide 296 random bits,
                    // exceeding the P0 requirement without a new dependency.
                    (0..4)
                        .map(|_| uuid::Uuid::now_v7().simple().to_string())
                        .collect::<String>()
                });
                Self(value)
            })
            .clone()
    }

    pub fn expose_for_desktop_bridge(&self) -> String {
        self.0.clone()
    }

    fn matches(&self, candidate: &str) -> bool {
        constant_time_eq(self.0.as_bytes(), candidate.as_bytes())
    }
}

impl fmt::Debug for SessionToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("<redacted studio session>")
    }
}

pub async fn require_session(
    State(expected): State<SessionToken>,
    request: Request,
    next: Next,
) -> Response {
    if request.method() == Method::OPTIONS || !requires_session(request.uri().path()) {
        return next.run(request).await;
    }
    let authorized = request
        .headers()
        .get(SESSION_HEADER)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|candidate| expected.matches(candidate));
    if authorized {
        next.run(request).await
    } else {
        (
            StatusCode::UNAUTHORIZED,
            [(header::CONTENT_TYPE, "application/json")],
            r#"{"error":"Studio session authorization is required."}"#,
        )
            .into_response()
    }
}

fn requires_session(path: &str) -> bool {
    ["/v1/", "/setup/", "/engine/"]
        .iter()
        .any(|prefix| path.starts_with(prefix))
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    let mut difference = left.len() ^ right.len();
    let size = left.len().max(right.len());
    for index in 0..size {
        difference |= usize::from(
            left.get(index).copied().unwrap_or_default()
                ^ right.get(index).copied().unwrap_or_default(),
        );
    }
    difference == 0
}

#[derive(Debug)]
pub enum ImageProxyError {
    InvalidUrl,
    ForbiddenDestination,
    TooManyRedirects,
    Transport,
    UpstreamStatus,
    UnsupportedType,
    TooLarge,
    TypeMismatch,
}

impl fmt::Display for ImageProxyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidUrl => "invalid public image URL",
            Self::ForbiddenDestination => "image destination is not a public HTTP endpoint",
            Self::TooManyRedirects => "image request exceeded the redirect limit",
            Self::Transport => "image request failed",
            Self::UpstreamStatus => "image server returned an unsuccessful status",
            Self::UnsupportedType => "image server returned an unsupported content type",
            Self::TooLarge => "image exceeds the 20 MiB proxy limit",
            Self::TypeMismatch => "image bytes do not match the declared content type",
        })
    }
}

pub async fn fetch_public_image(input: &str) -> Result<(String, Vec<u8>), ImageProxyError> {
    let mut url = reqwest::Url::parse(input).map_err(|_| ImageProxyError::InvalidUrl)?;
    for redirect_count in 0..=MAX_REDIRECTS {
        let (host, addresses) = validate_and_resolve(&url).await?;
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(5))
            .read_timeout(Duration::from_secs(15))
            .timeout(Duration::from_secs(30))
            .resolve_to_addrs(&host, &addresses)
            .build()
            .map_err(|_| ImageProxyError::Transport)?;
        let response = client
            .get(url.clone())
            .header(
                header::ACCEPT,
                "image/avif,image/webp,image/png,image/jpeg,image/gif",
            )
            .header(header::ACCEPT_ENCODING, "identity")
            .send()
            .await
            .map_err(|_| ImageProxyError::Transport)?;
        if response.status().is_redirection() {
            if redirect_count == MAX_REDIRECTS {
                return Err(ImageProxyError::TooManyRedirects);
            }
            let location = response
                .headers()
                .get(header::LOCATION)
                .and_then(|value| value.to_str().ok())
                .ok_or(ImageProxyError::InvalidUrl)?;
            url = url
                .join(location)
                .map_err(|_| ImageProxyError::InvalidUrl)?;
            continue;
        }
        if !response.status().is_success() {
            return Err(ImageProxyError::UpstreamStatus);
        }
        if response
            .headers()
            .get(header::CONTENT_ENCODING)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| !value.trim().is_empty() && value.trim() != "identity")
        {
            return Err(ImageProxyError::UnsupportedType);
        }
        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(normalized_mime)
            .filter(|value| ALLOWED_IMAGE_TYPES.contains(&value.as_str()))
            .ok_or(ImageProxyError::UnsupportedType)?;
        if response
            .content_length()
            .is_some_and(|size| size as usize > MAX_IMAGE_BYTES)
        {
            return Err(ImageProxyError::TooLarge);
        }
        let mut stream = response.bytes_stream();
        let mut bytes = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| ImageProxyError::Transport)?;
            if bytes.len().saturating_add(chunk.len()) > MAX_IMAGE_BYTES {
                return Err(ImageProxyError::TooLarge);
            }
            bytes.extend_from_slice(&chunk);
        }
        if !image_magic_matches(&content_type, &bytes) {
            return Err(ImageProxyError::TypeMismatch);
        }
        return Ok((content_type, bytes));
    }
    Err(ImageProxyError::TooManyRedirects)
}

async fn validate_and_resolve(
    url: &reqwest::Url,
) -> Result<(String, Vec<SocketAddr>), ImageProxyError> {
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(ImageProxyError::ForbiddenDestination);
    }
    let host = url.host_str().ok_or(ImageProxyError::InvalidUrl)?;
    // reqwest may canonicalize a trailing-dot hostname differently from the
    // key passed to `resolve_to_addrs`, which would silently discard DNS
    // pinning. Reject it instead of attempting a second normalization.
    if host.ends_with('.') {
        return Err(ImageProxyError::ForbiddenDestination);
    }
    let host = host.to_owned();
    if host.eq_ignore_ascii_case("localhost") || host.ends_with(".localhost") {
        return Err(ImageProxyError::ForbiddenDestination);
    }
    let port = url
        .port_or_known_default()
        .ok_or(ImageProxyError::InvalidUrl)?;
    if !matches!(port, 80 | 443) {
        return Err(ImageProxyError::ForbiddenDestination);
    }
    let lookup_host = host.clone();
    let addresses = tokio::task::spawn_blocking(move || {
        (lookup_host.as_str(), port)
            .to_socket_addrs()
            .map(|items| items.collect::<Vec<_>>())
    })
    .await
    .map_err(|_| ImageProxyError::Transport)?
    .map_err(|_| ImageProxyError::Transport)?;
    if addresses.is_empty() || addresses.iter().any(|address| !is_public_ip(address.ip())) {
        return Err(ImageProxyError::ForbiddenDestination);
    }
    Ok((host, addresses))
}

fn is_public_ip(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(value) => is_public_ipv4(value),
        IpAddr::V6(value) => is_public_ipv6(value),
    }
}

fn is_public_ipv4(value: Ipv4Addr) -> bool {
    let [a, b, c, _] = value.octets();
    !(a == 0
        || a == 10
        || a == 127
        || (a == 100 && (64..=127).contains(&b))
        || (a == 169 && b == 254)
        || (a == 172 && (16..=31).contains(&b))
        || (a == 192 && b == 168)
        || (a == 192 && b == 0 && c == 0)
        || (a == 192 && b == 0 && c == 2)
        || (a == 192 && b == 88 && c == 99)
        || (a == 198 && (b == 18 || b == 19))
        || (a == 198 && b == 51 && c == 100)
        || (a == 203 && b == 0 && c == 113)
        || (224..=255).contains(&a))
}

fn is_public_ipv6(value: Ipv6Addr) -> bool {
    if let Some(mapped) = value.to_ipv4_mapped() {
        return is_public_ipv4(mapped);
    }
    let segments = value.segments();
    let is_nat64_well_known = segments[..6] == [0x0064, 0xff9b, 0, 0, 0, 0];
    let is_nat64_local_use = segments[0] == 0x0064 && segments[1] == 0xff9b && segments[2] == 1;
    let is_6to4 = segments[0] == 0x2002;
    let is_teredo = segments[0] == 0x2001 && segments[1] == 0;
    !(value.is_unspecified()
        || value.is_loopback()
        || value.is_multicast()
        || (segments[0] & 0xfe00) == 0xfc00
        || (segments[0] & 0xffc0) == 0xfe80
        || (segments[0] == 0x2001 && segments[1] == 0x0db8)
        || is_nat64_well_known
        || is_nat64_local_use
        || is_6to4
        || is_teredo)
}

fn normalized_mime(value: &str) -> String {
    value
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase()
}

fn image_magic_matches(mime: &str, bytes: &[u8]) -> bool {
    match mime {
        "image/jpeg" => bytes.starts_with(&[0xff, 0xd8, 0xff]),
        "image/png" => bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
        "image/gif" => bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a"),
        "image/webp" => bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP",
        "image/avif" => {
            bytes.len() >= 12
                && &bytes[4..8] == b"ftyp"
                && bytes[8..12]
                    .windows(4)
                    .any(|brand| brand == b"avif" || brand == b"avis")
        }
        _ => false,
    }
}

pub fn redact_secrets(message: impl AsRef<str>) -> String {
    let mut redacted = message.as_ref().replace('\r', " ").replace('\n', " ");
    for name in [
        "MINIMAX_STUDIO_SESSION_TOKEN",
        "MUSIC_MAKER_OMNIBRIDGE_GATEWAY_KEY",
        "OPENROUTER_API_KEY",
    ] {
        if let Ok(secret) = env::var(name) {
            if secret.trim().len() >= 6 {
                redacted = redacted.replace(secret.trim(), "<redacted>");
            }
        }
    }
    for marker in [
        "Authorization: Bearer ",
        "Authorization:",
        "X-Task-Token:",
        "X-Studio-Session:",
        "X-Gateway-Key:",
        "Gateway-Key:",
        "Gateway Key:",
        "X-Provider-Key:",
        "Provider-Key:",
        "Provider Key:",
        "Bearer ",
        "task_token=",
        "task_token:",
        "api_key=",
        "api_key:",
        "token=",
        "token:",
    ] {
        redacted = redact_after_marker(redacted, marker);
    }
    redact_url_queries(redacted)
}

fn redact_after_marker(mut value: String, marker: &str) -> String {
    let mut start = 0;
    while let Some(relative) = value[start..]
        .to_ascii_lowercase()
        .find(&marker.to_ascii_lowercase())
    {
        let marker_end = start + relative + marker.len();
        let leading_whitespace = value[marker_end..]
            .chars()
            .take_while(|character| character.is_whitespace())
            .map(char::len_utf8)
            .sum::<usize>();
        let value_start = marker_end + leading_whitespace;
        let value_end = value[value_start..]
            .find(|character: char| {
                character.is_whitespace() || matches!(character, '&' | ',' | ';' | '"')
            })
            .map(|length| value_start + length)
            .unwrap_or(value.len());
        value.replace_range(value_start..value_end, "<redacted>");
        start = value_start + "<redacted>".len();
    }
    value
}

fn redact_url_queries(value: String) -> String {
    value
        .split_whitespace()
        .map(|word| {
            let trimmed = word.trim_matches(|character: char| {
                matches!(character, '(' | ')' | '[' | ']' | '"' | '\'')
            });
            if (trimmed.starts_with("http://") || trimmed.starts_with("https://"))
                && trimmed.contains('?')
            {
                reqwest::Url::parse(trimmed)
                    .map(|mut url| {
                        url.set_query(Some("redacted"));
                        url.set_fragment(None);
                        word.replace(trimmed, url.as_str())
                    })
                    .unwrap_or_else(|_| word.to_owned())
            } else {
                word.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_tokens_are_redacted_and_compared_exactly() {
        let token = SessionToken("01234567890123456789012345678901".to_owned());
        assert!(token.matches("01234567890123456789012345678901"));
        assert!(!token.matches("01234567890123456789012345678900"));
        assert!(!format!("{token:?}").contains("012345"));
        assert!(requires_session("/v1/music/jobs"));
        assert!(requires_session("/setup/status"));
        assert!(requires_session("/engine/options"));
        assert!(!requires_session("/health"));
        assert!(!requires_session("/editor/index.html"));
    }

    #[test]
    fn private_metadata_and_reserved_networks_are_blocked() {
        for address in [
            "127.0.0.1",
            "10.0.0.1",
            "169.254.169.254",
            "192.168.1.2",
            "192.88.99.1",
            "100.64.0.1",
            "203.0.113.8",
        ] {
            assert!(
                !is_public_ip(address.parse().unwrap()),
                "{address} must be blocked"
            );
        }
        assert!(is_public_ip("1.1.1.1".parse().unwrap()));
    }

    #[test]
    fn ipv6_transition_networks_are_blocked() {
        for address in [
            "64:ff9b::7f00:1",
            "64:ff9b:1::7f00:1",
            "2002:7f00:0001::",
            "2001:0000:4136:e378:8000:63bf:3fff:fdd2",
        ] {
            assert!(
                !is_public_ip(address.parse().unwrap()),
                "{address} must be blocked"
            );
        }
        assert!(is_public_ip("2606:4700:4700::1111".parse().unwrap()));
    }

    #[tokio::test]
    async fn trailing_dot_hosts_are_rejected_before_dns_pinning() {
        let url = reqwest::Url::parse("https://example.com./cover.png").unwrap();
        assert!(matches!(
            validate_and_resolve(&url).await,
            Err(ImageProxyError::ForbiddenDestination)
        ));
    }

    #[test]
    fn errors_remove_credentials_and_signed_url_queries() {
        let redacted = redact_secrets(concat!(
            "Authorization: Bearer auth-secret X-Task-Token: task-secret ",
            "X-Studio-Session: studio-secret Gateway-Key: gateway-secret ",
            "Provider-Key: provider-secret task_token=private ",
            "https://cdn.example/a.png?sig=secret"
        ));
        for secret in [
            "auth-secret",
            "task-secret",
            "studio-secret",
            "gateway-secret",
            "provider-secret",
            "private",
            "sig=secret",
        ] {
            assert!(!redacted.contains(secret), "secret leaked: {secret}");
        }
        assert!(redacted.contains("redacted"));
    }
}
