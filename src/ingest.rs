//! Explicit, bounded source adapters. No network traffic during normal random reads.
use crate::domain::{Category, NewSaying};
use anyhow::{anyhow, bail, Context, Result};
use reqwest::blocking::Client;
use scraper::{Html, Selector};
use serde::Deserialize;
use std::io::Read;
use std::sync::Once;
use std::time::Duration;
use url::Url;

const VIRTUAL_BASE: &str =
    "https://www.cervantesvirtual.com/portales/refranes_dichos_proverbios_citas_adivinanzas/";
const CVC_BASE: &str = "https://cvc.cervantes.es/lengua/refranero/";
const MAX_BODY: usize = 8 * 1024 * 1024;

/// Browser-oriented HTTP client with finite timeouts.
pub fn http_client() -> Result<Client> {
    http_client_with_timeout(Duration::from_secs(20))
}

/// Install the ring TLS crypto provider once per process.
///
/// reqwest is built with `rustls-no-provider`, so it does not select a crypto
/// backend on its own. The application must install one before building a TLS
/// client, otherwise reqwest has no provider to fall back on.
fn install_crypto_provider() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        if rustls::crypto::CryptoProvider::get_default().is_none() {
            let _ = rustls::crypto::ring::default_provider().install_default();
        }
    });
}

/// Create a bounded HTTP client with a caller-selected request timeout.
pub fn http_client_with_timeout(timeout: Duration) -> Result<Client> {
    install_crypto_provider();
    Ok(Client::builder()
        .user_agent("refranero-cli/0.1 (personal opt-in import; contact: local user)")
        .timeout(timeout)
        .redirect(reqwest::redirect::Policy::limited(5))
        .build()?)
}

/// Download a text/html page with an upper body-size limit.
fn fetch_html(client: &Client, url: &str) -> Result<String> {
    let response = client.get(url).send()?.error_for_status()?;
    // Bound network reads rather than downloading an arbitrarily large response first.
    let mut bytes = Vec::new();
    response
        .take((MAX_BODY + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_BODY {
        bail!("HTML too large: more than {MAX_BODY} bytes");
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// Read up to `limit` entries from one current Cervantes Virtual category page.
pub fn import_virtual(client: &Client, category: Category, limit: usize) -> Result<Vec<NewSaying>> {
    if category == Category::Paremia {
        bail!("use an explicit modern category: refran, proverbio, dicho, adivinanza or cita");
    }
    let url = format!("{VIRTUAL_BASE}{}/", category.url_segment());
    let html = fetch_html(client, &url).with_context(|| format!("retrieving {url}"))?;
    Ok(parse_virtual(&html, category, &url, limit))
}

/// Extract saying text and a validated per-entry citation from blockquotes.
/// Fall back to the category page when a citation is missing or unsafe.
fn parse_virtual(html: &str, category: Category, url: &str, limit: usize) -> Vec<NewSaying> {
    let doc = Html::parse_document(html);
    let selector = Selector::parse("blockquote").expect("static CSS selector");
    doc.select(&selector)
        .filter_map(|el| {
            let text = normalize(&el.text().collect::<String>());
            if text.is_empty() || text.chars().count() > 3000 {
                return None;
            }
            let source_url = el
                .value()
                .attr("cite")
                .and_then(verified_virtual_citation_url)
                .unwrap_or_else(|| url.to_owned());
            Some(NewSaying {
                text,
                category: category.as_str().into(),
                language: "es".into(),
                source: "cervantes_virtual".into(),
                source_url,
                original_url: Some(url.to_owned()),
                captured_at: None,
                group_id: None,
            })
        })
        .take(limit)
        .collect()
}

fn verified_virtual_citation_url(raw: &str) -> Option<String> {
    let mut citation = Url::parse(raw).ok()?;
    if !matches!(citation.scheme(), "http" | "https")
        || !matches!(
            citation.host_str(),
            Some("www.cervantesvirtual.com") | Some("cervantesvirtual.com")
        )
        || !citation.username().is_empty()
        || citation.password().is_some()
    {
        return None;
    }
    let has_valid_port = matches!(
        (citation.scheme(), citation.port()),
        ("http", None | Some(80)) | ("https", None | Some(443))
    );
    if !has_valid_port {
        return None;
    }
    citation.set_port(None).ok()?;
    citation.set_scheme("https").ok()?;
    Some(citation.to_string())
}

#[derive(Debug, Deserialize)]
struct AvailabilityResponse {
    archived_snapshots: Snapshots,
}
#[derive(Debug, Default, Deserialize)]
struct Snapshots {
    closest: Option<Snapshot>,
}
#[derive(Debug, Deserialize)]
struct Snapshot {
    available: bool,
    url: String,
    timestamp: String,
    status: String,
}

/// Find a historical alphabetical index through the Internet Archive availability API.
/// The API returns only the nearest capture; it does NOT prove that all linked fichas exist.
pub fn import_wayback(
    client: &Client,
    letter: char,
    year: u16,
    limit: usize,
) -> Result<Vec<NewSaying>> {
    if !letter.is_ascii_alphabetic() {
        bail!("--letter must be one ASCII alphabetic character");
    }
    if !(1996..=2100).contains(&year) {
        bail!("--year must be between 1996 and 2100");
    }
    let index_url = format!(
        "{CVC_BASE}listado.aspx?letra={}",
        letter.to_ascii_uppercase()
    );
    let response: AvailabilityResponse = client
        .get("https://archive.org/wayback/available")
        .query(&[
            ("url", index_url.as_str()),
            ("timestamp", &year.to_string()),
        ])
        .send()?
        .error_for_status()?
        .json()?;
    let capture = response
        .archived_snapshots
        .closest
        .ok_or_else(|| anyhow!("no Wayback capture available for {index_url}"))?;
    if !capture.available || capture.status != "200" {
        bail!("nearest Wayback capture is not an available HTTP 200 page");
    }
    // Never follow an API-provided arbitrary host.
    let mut replay = Url::parse(&capture.url)?;
    if replay.host_str() != Some("web.archive.org") {
        bail!("unexpected archive host in availability response");
    }
    replay
        .set_scheme("https")
        .map_err(|_| anyhow!("invalid Wayback replay scheme"))?;
    let replay_url = replay.to_string();
    let html =
        fetch_html(client, &replay_url).with_context(|| format!("retrieving {replay_url}"))?;
    Ok(parse_wayback_index(&html, &capture.timestamp, limit))
}

/// Extract Spanish paremia anchors from archived CVC listings, without fabricating translations.
fn parse_wayback_index(html: &str, timestamp: &str, limit: usize) -> Vec<NewSaying> {
    let doc = Html::parse_document(html);
    let links = Selector::parse("a[href]").expect("static CSS selector");
    let base = Url::parse(CVC_BASE).expect("static base URL");
    doc.select(&links)
        .filter_map(|el| {
            let href = el.value().attr("href")?;
            if !href.to_ascii_lowercase().contains("ficha.aspx") {
                return None;
            }
            let original = normalize_cvc_url(href, &base)?;
            let par = original
                .query_pairs()
                .find(|(k, _)| k.eq_ignore_ascii_case("Par"))?
                .1
                .to_string();
            if par.is_empty() || !par.chars().all(|c| c.is_ascii_digit()) {
                return None;
            }
            let text = normalize(&el.text().collect::<String>());
            if text.is_empty() || text.chars().count() > 500 {
                return None;
            }
            let canonical = original.to_string();
            Some(NewSaying {
                text,
                category: "paremia".into(), // CVC listing contains different subtypes.
                language: "es".into(),
                source: "cvc_wayback".into(),
                source_url: format!("https://web.archive.org/web/{timestamp}/{canonical}"),
                original_url: Some(canonical),
                captured_at: Some(timestamp.into()),
                group_id: Some(format!("cvc:{par}")),
            })
        })
        .take(limit)
        .collect()
}

/// Reconstruct the original CVC URL despite Wayback's rewritten anchor paths.
fn normalize_cvc_url(href: &str, base: &Url) -> Option<Url> {
    let lowered = href.to_ascii_lowercase();
    let original_start = lowered
        .find("https://cvc.cervantes.es/")
        .or_else(|| lowered.find("http://cvc.cervantes.es/"));
    let candidate = match original_start {
        Some(position) => Url::parse(&href[position..]).ok()?,
        None => base.join(href).ok()?,
    };
    if candidate.host_str()? != "cvc.cervantes.es" {
        return None;
    }
    if !candidate
        .path()
        .to_ascii_lowercase()
        .ends_with("/ficha.aspx")
    {
        return None;
    }
    let mut candidate = candidate;
    candidate.set_scheme("https").ok()?;
    Some(candidate)
}

/// Collapse whitespace without changing the words of the original entry.
fn normalize(raw: &str) -> String {
    raw.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installs_a_tls_crypto_provider_before_building_clients() {
        install_crypto_provider();
        assert!(rustls::crypto::CryptoProvider::get_default().is_some());
    }

    #[test]
    fn parses_current_portal_blockquotes() {
        let html = r#"<h1>Refranes</h1><blockquote> A falta de <b>pan</b>, buenas son tortas. </blockquote>"#;
        let entries = parse_virtual(html, Category::Refran, "https://example.org/", 3);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].text, "A falta de pan, buenas son tortas.");
    }

    #[test]
    fn uses_validated_per_entry_citation_and_keeps_category_provenance() {
        let category_url = "https://www.cervantesvirtual.com/portales/refranes_dichos_proverbios_citas_adivinanzas/refran/";
        let html = r#"<blockquote cite="http://www.cervantesvirtual.com/obra-visor/refranes-alusivos-al-tiempo/html/">No hay sábado sin sol.</blockquote>"#;

        let entries = parse_virtual(html, Category::Refran, category_url, 1);

        assert_eq!(entries.len(), 1);
        assert_eq!(
            entries[0].source_url,
            "https://www.cervantesvirtual.com/obra-visor/refranes-alusivos-al-tiempo/html/"
        );
        assert_eq!(entries[0].original_url.as_deref(), Some(category_url));
    }

    #[test]
    fn falls_back_to_category_for_relative_citations() {
        let category_url = "https://www.cervantesvirtual.com/portales/refranes_dichos_proverbios_citas_adivinanzas/refran/";
        let html = r#"<blockquote cite="../../obra-visor/refranes-alusivos-al-tiempo/html/">No hay sábado sin sol.</blockquote>"#;

        let entries = parse_virtual(html, Category::Refran, category_url, 1);

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].source_url, category_url);
    }

    #[test]
    fn rejects_citation_ports_that_do_not_match_the_scheme() {
        let category_url = "https://www.cervantesvirtual.com/portales/refranes_dichos_proverbios_citas_adivinanzas/refran/";
        let html = r#"
            <blockquote cite="https://www.cervantesvirtual.com:80/obra-visor/one/">Uno.</blockquote>
            <blockquote cite="http://www.cervantesvirtual.com:443/obra-visor/two/">Dos.</blockquote>
        "#;

        let entries = parse_virtual(html, Category::Refran, category_url, 2);

        assert_eq!(entries.len(), 2);
        assert!(entries.iter().all(|entry| entry.source_url == category_url));
    }

    #[test]
    fn falls_back_to_category_for_untrusted_or_invalid_citations() {
        let category_url = "https://www.cervantesvirtual.com/portales/refranes_dichos_proverbios_citas_adivinanzas/refran/";
        let html = r#"
            <blockquote cite="https://evil.example/redirect">Uno.</blockquote>
            <blockquote cite="javascript:alert(1)">Dos.</blockquote>
            <blockquote>Tres.</blockquote>
        "#;

        let entries = parse_virtual(html, Category::Refran, category_url, 3);

        assert_eq!(entries.len(), 3);
        for entry in entries {
            assert_eq!(entry.source_url, category_url);
            assert_eq!(entry.original_url.as_deref(), Some(category_url));
        }
    }

    #[test]
    fn parses_original_and_wayback_rewritten_fichas() {
        let html = r#"<a href="/lengua/refranero/Ficha.aspx?Lng=0&amp;Par=58122">A quien madruga, Dios le ayuda</a>
          <a href="/web/20190101120000/https://cvc.cervantes.es/lengua/refranero/ficha.aspx?Lng=0&amp;Par=58200">Otro refrán</a>
          <a href="https://malicious.example/ficha.aspx?Par=123">Ignore me</a>"#;
        let entries = parse_wayback_index(html, "20190101120000", 10);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].group_id.as_deref(), Some("cvc:58122"));
        assert_eq!(entries[1].group_id.as_deref(), Some("cvc:58200"));
        assert_eq!(entries[0].category, "paremia");
    }
}
