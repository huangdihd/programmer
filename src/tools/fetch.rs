// Copyright (C) 2026 huangdihd
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.

//! The `fetch` tool: retrieve a URL and return its content as text.
//!
//! HTML is converted to plain text; textual content types pass through
//! unchanged. Every hop (including each redirect) is validated against
//! private/internal addresses before it is requested, so a prompt-injected
//! URL cannot be used to probe loopback services, LAN hosts, or cloud
//! metadata endpoints from the user's machine.

use async_openai::types::responses::Tool;
use serde::Deserialize;
use serde_json::json;
use std::net::IpAddr;
use std::time::Duration;

use super::function_tool;

pub const NAME: &str = "fetch";

/// Hard cap on downloaded body bytes; the rest is discarded.
const MAX_BODY_BYTES: usize = 5 * 1024 * 1024;
/// Maximum redirect hops followed (each hop is re-validated).
const MAX_REDIRECTS: usize = 5;
/// Whole-request timeout.
const TIMEOUT_SECS: u64 = 20;
/// Column width used when rendering HTML to text.
const HTML_WIDTH: usize = 100;
/// Default and maximum characters returned per call. The maximum stays below
/// the global tool-output cap so the paging footer survives intact instead of
/// being cut by the generic middle-truncation.
const DEFAULT_MAX_LENGTH: usize = 6000;
const MAX_MAX_LENGTH: usize = 7000;

pub fn tool() -> Tool {
    function_tool(
        NAME,
        "Fetch an http(s) URL and return its content as text. HTML pages are \
         converted to plain text (set raw=true for the original HTML). Long \
         pages are returned in chunks — follow the trailing note's \
         start_index to continue reading. Private/internal addresses are \
         refused.",
        json!({
            "url": {
                "type": "string",
                "description": "The http(s) URL to fetch."
            },
            "max_length": {
                "type": "integer",
                "description": "Maximum characters to return (default 6000, max 7000)."
            },
            "start_index": {
                "type": "integer",
                "description": "Character offset to start from, for paging through long content (default 0)."
            },
            "raw": {
                "type": "boolean",
                "description": "Return raw HTML instead of the text conversion (default false)."
            }
        }),
        &["url"],
    )
}

#[derive(Deserialize)]
struct Args {
    url: String,
    #[serde(default)]
    max_length: Option<usize>,
    #[serde(default)]
    start_index: Option<usize>,
    #[serde(default)]
    raw: Option<bool>,
}

pub async fn run(arguments: &str) -> Result<String, String> {
    let args: Args = match serde_json::from_str(arguments) {
        Ok(a) => a,
        Err(e) => return Err(format!("error: invalid arguments: {e}")),
    };
    let max_length = args
        .max_length
        .unwrap_or(DEFAULT_MAX_LENGTH)
        .clamp(1, MAX_MAX_LENGTH);
    let start_index = args.start_index.unwrap_or(0);
    let raw = args.raw.unwrap_or(false);

    let mut url = reqwest::Url::parse(&args.url)
        .map_err(|e| format!("error: invalid URL '{}': {e}", args.url))?;

    let client = reqwest::Client::builder()
        // Redirects are followed manually below so each hop's target is
        // validated before it is requested.
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(TIMEOUT_SECS))
        .user_agent(concat!("programmer/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| format!("error: cannot build HTTP client: {e}"))?;

    let mut response = None;
    for _ in 0..=MAX_REDIRECTS {
        validate_url(&url).await?;
        let resp = client
            .get(url.clone())
            .send()
            .await
            .map_err(|e| format!("error: request failed: {e}"))?;
        if resp.status().is_redirection() {
            let location = resp
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| "error: redirect without a Location header".to_string())?;
            url = url
                .join(location)
                .map_err(|e| format!("error: invalid redirect target '{location}': {e}"))?;
            continue;
        }
        response = Some(resp);
        break;
    }
    let mut response =
        response.ok_or_else(|| format!("error: more than {MAX_REDIRECTS} redirects"))?;

    let status = response.status();
    if !status.is_success() {
        return Err(format!("error: HTTP {status} fetching {url}"));
    }
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();

    // Stream the body so an oversized response is cut off rather than
    // buffered whole.
    let mut body: Vec<u8> = Vec::new();
    let mut body_truncated = false;
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| format!("error: reading response body: {e}"))?
    {
        body.extend_from_slice(&chunk);
        if body.len() > MAX_BODY_BYTES {
            body.truncate(MAX_BODY_BYTES);
            body_truncated = true;
            break;
        }
    }

    let is_html = content_type.contains("text/html") || content_type.contains("xhtml");
    let text = if is_html && !raw {
        html2text::from_read(body.as_slice(), HTML_WIDTH)
            .map_err(|e| format!("error: cannot convert HTML to text: {e}"))?
    } else if is_texty(&content_type) {
        String::from_utf8_lossy(&body).into_owned()
    } else {
        return Err(format!(
            "error: unsupported content type '{content_type}' — only text-based \
             content can be fetched"
        ));
    };

    Ok(page(&text, start_index, max_length, &url, body_truncated))
}

/// Whether a content type is safe to return as text. An absent content type
/// is treated as text (common for raw files and misconfigured servers).
fn is_texty(content_type: &str) -> bool {
    content_type.is_empty()
        || content_type.starts_with("text/")
        || content_type.contains("json")
        || content_type.contains("xml")
        || content_type.contains("javascript")
        || content_type.contains("x-sh")
        || content_type.contains("html")
        || content_type.contains("yaml")
        || content_type.contains("toml")
        || content_type.contains("csv")
        || content_type.contains("x-www-form-urlencoded")
}

/// Reject URLs that would reach private or internal addresses.
///
/// Checked per hop, right before the request. A DNS answer could still change
/// between this check and the connect (rebinding); accepted as residual risk
/// for a local development tool.
async fn validate_url(url: &reqwest::Url) -> Result<(), String> {
    match url.scheme() {
        "http" | "https" => {}
        s => {
            return Err(format!(
                "error: unsupported scheme '{s}' — only http and https"
            ));
        }
    }
    let host = url
        .host_str()
        .ok_or_else(|| "error: URL has no host".to_string())?;
    // `host_str` keeps the brackets around IPv6 literals.
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    let port = url.port_or_known_default().unwrap_or(443);

    if let Ok(ip) = bare.parse::<IpAddr>() {
        if is_forbidden_ip(ip) {
            return Err(format!(
                "error: refusing to fetch private/internal address {host}"
            ));
        }
        return Ok(());
    }

    let addrs = tokio::net::lookup_host((bare, port))
        .await
        .map_err(|e| format!("error: cannot resolve {host}: {e}"))?;
    let mut resolved_any = false;
    for addr in addrs {
        resolved_any = true;
        if is_forbidden_ip(addr.ip()) {
            return Err(format!(
                "error: refusing to fetch {host} — it resolves to the \
                 private/internal address {}",
                addr.ip()
            ));
        }
    }
    if !resolved_any {
        return Err(format!("error: cannot resolve {host}"));
    }
    Ok(())
}

/// Loopback, LAN, link-local (incl. cloud metadata), CGNAT, and unspecified
/// addresses — everything a prompt-injected URL must not reach.
fn is_forbidden_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                // CGNAT 100.64.0.0/10
                || (o[0] == 100 && (64..128).contains(&o[1]))
        }
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_forbidden_ip(IpAddr::V4(v4));
            }
            let seg0 = v6.segments()[0];
            v6.is_loopback()
                || v6.is_unspecified()
                // fc00::/7 unique-local
                || (seg0 & 0xfe00) == 0xfc00
                // fe80::/10 link-local
                || (seg0 & 0xffc0) == 0xfe80
        }
    }
}

/// Slice `text` by characters for paging and append a footer with the final
/// URL and, when there is more content, the next `start_index` to request.
fn page(text: &str, start: usize, max_length: usize, url: &reqwest::Url, body_truncated: bool) -> String {
    let total = text.chars().count();
    if total == 0 {
        return format!("(empty response)\n[fetched {url}]");
    }
    if start >= total {
        return format!(
            "(start_index {start} is beyond the end — content is {total} chars)\n[fetched {url}]"
        );
    }
    let slice: String = text.chars().skip(start).take(max_length).collect();
    let end = start + slice.chars().count();
    let mut out = slice;
    out.push_str(&format!("\n[fetched {url}; chars {start}..{end} of {total}"));
    if end < total {
        out.push_str(&format!("; continue with start_index={end}"));
    }
    if body_truncated {
        out.push_str("; body truncated at the 5MB download cap");
    }
    out.push(']');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn rejects_invalid_and_non_http_urls() {
        let err = run(r#"{"url":"not a url"}"#).await.expect_err("invalid URL");
        assert!(err.starts_with("error: invalid URL"), "got: {err}");

        for url in ["file:///etc/passwd", "ftp://example.com/x"] {
            let err = run(&format!(r#"{{"url":"{url}"}}"#))
                .await
                .expect_err("non-http scheme");
            assert!(err.contains("unsupported scheme"), "got: {err}");
        }
    }

    #[tokio::test]
    async fn rejects_private_and_loopback_hosts() {
        for url in [
            "http://127.0.0.1/",
            "http://localhost:8080/",
            "http://192.168.1.1/admin",
            "http://10.0.0.5/",
            "http://169.254.169.254/latest/meta-data/",
            "http://[::1]/",
        ] {
            let err = run(&format!(r#"{{"url":"{url}"}}"#))
                .await
                .expect_err("private address must be refused");
            assert!(
                err.contains("private/internal"),
                "url {url} got: {err}"
            );
        }
    }

    #[test]
    fn forbidden_ip_ranges() {
        for ip in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.0.1",
            "169.254.169.254",
            "100.64.0.1",
            "0.0.0.0",
            "::1",
            "fc00::1",
            "fe80::1",
            "::ffff:127.0.0.1",
        ] {
            assert!(is_forbidden_ip(ip.parse().unwrap()), "{ip} should be forbidden");
        }
        for ip in ["8.8.8.8", "1.1.1.1", "2606:4700:4700::1111", "100.128.0.1"] {
            assert!(!is_forbidden_ip(ip.parse().unwrap()), "{ip} should be allowed");
        }
    }

    #[test]
    fn paging_slices_and_reports_continuation() {
        let url = reqwest::Url::parse("https://example.com/x").unwrap();
        let text = "abcdefghij";
        let out = page(text, 0, 4, &url, false);
        assert!(out.starts_with("abcd\n"), "got: {out}");
        assert!(out.contains("chars 0..4 of 10"), "got: {out}");
        assert!(out.contains("start_index=4"), "got: {out}");

        let out = page(text, 4, 100, &url, false);
        assert!(out.starts_with("efghij\n"), "got: {out}");
        assert!(!out.contains("start_index="), "got: {out}");

        let out = page(text, 50, 10, &url, false);
        assert!(out.contains("beyond the end"), "got: {out}");
    }

    #[test]
    fn html_converts_to_text() {
        let html = b"<html><body><h1>Title</h1><p>Hello <b>world</b></p></body></html>";
        // html2text renders markdown-ish text ("# Title", "**world**") —
        // assert on the words, not the exact formatting.
        let text = html2text::from_read(&html[..], HTML_WIDTH).expect("convert");
        assert!(text.contains("Title"), "got: {text}");
        assert!(text.contains("Hello"), "got: {text}");
        assert!(text.contains("world"), "got: {text}");
        assert!(!text.contains("<p>"), "tags must be stripped, got: {text}");
    }
}
