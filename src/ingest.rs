//! Explicit, bounded source adapters. No network traffic during normal random reads.
use crate::domain::{Category, NewSaying};
use anyhow::{anyhow, bail, Context, Result};
use reqwest::blocking::Client;
use scraper::{Html, Selector};
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
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
    read_html_response(response)
}

fn fetch_wayback_html(client: &Client, replay_url: &str) -> Result<String> {
    let response = client.get(replay_url).send()?.error_for_status()?;
    validate_wayback_final_url(response.url())?;
    read_html_response(response)
}

fn read_html_response(response: reqwest::blocking::Response) -> Result<String> {
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

struct WaybackReplay {
    replay_url: Url,
    timestamp: String,
}

fn validate_wayback_snapshot(snapshot: Option<Snapshot>) -> Result<Option<WaybackReplay>> {
    let Some(snapshot) = snapshot else {
        return Ok(None);
    };
    if !snapshot.available || snapshot.status != "200" {
        return Ok(None);
    }
    if snapshot.timestamp.len() != 14
        || !snapshot
            .timestamp
            .bytes()
            .all(|character| character.is_ascii_digit())
    {
        bail!("invalid Wayback capture timestamp");
    }

    let mut replay_url = Url::parse(&snapshot.url)?;
    if replay_url.host_str() != Some("web.archive.org")
        || !replay_url.username().is_empty()
        || replay_url.password().is_some()
    {
        bail!("unexpected archive host in availability response");
    }
    let valid_port = matches!(
        (replay_url.scheme(), replay_url.port()),
        ("http", None | Some(80)) | ("https", None | Some(443))
    );
    if !valid_port {
        bail!("unexpected port in Wayback replay URL");
    }
    replay_url
        .set_port(None)
        .map_err(|_| anyhow!("invalid Wayback replay port"))?;
    replay_url
        .set_scheme("https")
        .map_err(|_| anyhow!("invalid Wayback replay scheme"))?;

    Ok(Some(WaybackReplay {
        replay_url,
        timestamp: snapshot.timestamp,
    }))
}

fn validate_wayback_final_url(final_url: &Url) -> Result<()> {
    if final_url.host_str() != Some("web.archive.org") {
        bail!("Wayback replay redirected to an unexpected host");
    }
    Ok(())
}

fn find_wayback_capture(
    client: &Client,
    original_url: &Url,
    year: u16,
) -> Result<Option<WaybackReplay>> {
    let timestamp = year.to_string();
    let response: AvailabilityResponse = client
        .get("https://archive.org/wayback/available")
        .query(&[("url", original_url.as_str()), ("timestamp", &timestamp)])
        .send()?
        .error_for_status()?
        .json()?;
    validate_wayback_snapshot(response.archived_snapshots.closest)
}

fn fetch_wayback_ficha(
    client: &Client,
    original_url: &Url,
    year: u16,
) -> Result<Option<CapturedFicha>> {
    let Some(capture) = find_wayback_capture(client, original_url, year)? else {
        return Ok(None);
    };
    let replay_url = capture.replay_url;
    let html = fetch_wayback_html(client, replay_url.as_str())
        .with_context(|| format!("retrieving {replay_url}"))?;
    Ok(Some(CapturedFicha {
        html,
        replay_url,
        timestamp: capture.timestamp,
    }))
}

/// Import Spanish index entries and available language variants from archived fichas.
/// Each ficha uses its own nearest Wayback capture; the index capture does not guarantee
/// that any linked ficha or language variant is archived.
pub fn import_wayback(
    client: &Client,
    letter: char,
    year: u16,
    limit: usize,
) -> Result<WaybackImportReport> {
    if !letter.is_ascii_alphabetic() {
        bail!("--letter must be one ASCII alphabetic character");
    }
    if !(1996..=2100).contains(&year) {
        bail!("--year must be between 1996 and 2100");
    }
    let index_url = Url::parse(&format!(
        "{CVC_BASE}listado.aspx?letra={}",
        letter.to_ascii_uppercase()
    ))?;
    let capture = find_wayback_capture(client, &index_url, year)?
        .ok_or_else(|| anyhow!("no available HTTP 200 Wayback capture for {index_url}"))?;
    let html = fetch_html(client, capture.replay_url.as_str())
        .with_context(|| format!("retrieving {}", capture.replay_url))?;
    let roots = parse_wayback_index(&html, &capture.timestamp, limit);
    Ok(collect_wayback_fichas(
        roots,
        year,
        |original_url, target_year| fetch_wayback_ficha(client, original_url, target_year),
    ))
}

/// Extract Spanish paremia anchors from archived CVC listings, without fabricating translations.
fn parse_wayback_index(html: &str, timestamp: &str, limit: usize) -> Vec<NewSaying> {
    let doc = Html::parse_document(html);
    let links = Selector::parse("a[href]").expect("static CSS selector");
    let base = Url::parse(CVC_BASE).expect("static base URL");
    let candidates = doc
        .select(&links)
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
            let is_spanish = original
                .query_pairs()
                .find(|(key, _)| key.eq_ignore_ascii_case("Lng"))
                .is_some_and(|(_, value)| value == "0");
            let canonical = original.to_string();
            Some((
                NewSaying {
                    text,
                    category: "paremia".into(), // CVC listing contains different subtypes.
                    language: "es".into(),
                    source: "cvc_wayback".into(),
                    source_url: format!("https://web.archive.org/web/{timestamp}/{canonical}"),
                    original_url: Some(canonical),
                    captured_at: Some(timestamp.into()),
                    group_id: Some(format!("cvc:{par}")),
                },
                is_spanish,
            ))
        })
        .collect::<Vec<_>>();

    let mut seen_pars: HashMap<String, usize> = HashMap::new();
    let mut roots: Vec<(NewSaying, bool)> = Vec::new();
    for (entry, is_spanish) in candidates {
        let group_id = entry.group_id.clone().unwrap_or_default();
        if let Some(index) = seen_pars.get(&group_id).copied() {
            if is_spanish && !roots[index].1 {
                roots[index] = (entry, true);
            }
        } else {
            seen_pars.insert(group_id, roots.len());
            roots.push((entry, is_spanish));
        }
    }

    // The index is Spanish. Keep the first valid anchor when a Par has no Lng=0 anchor.
    roots
        .into_iter()
        .map(|(entry, _)| entry)
        .take(limit)
        .collect()
}

#[derive(Debug)]
struct WaybackLanguageLink {
    language: String,
    original_url: Url,
    par: String,
}

pub struct WaybackImportWarning {
    pub par: String,
    pub language: Option<String>,
    pub reason: String,
}

struct LanguageDiscovery {
    links: Vec<WaybackLanguageLink>,
    warnings: Vec<WaybackImportWarning>,
}

struct CapturedFicha {
    html: String,
    replay_url: Url,
    timestamp: String,
}

struct VariantCollection {
    entries: Vec<NewSaying>,
    available: usize,
    unavailable: usize,
    warnings: Vec<WaybackImportWarning>,
}

pub struct WaybackImportReport {
    pub entries: Vec<NewSaying>,
    pub roots_processed: usize,
    pub variants_available: usize,
    pub variants_unavailable: usize,
    /// Spanish `Par` IDs with a confirmed `Lng=0` capture in this import run.
    pub captured_spanish_pars: HashSet<String>,
    pub warnings: Vec<WaybackImportWarning>,
}

fn parse_wayback_language_links(html: &str, expected_par: &str) -> LanguageDiscovery {
    let doc = Html::parse_document(html);
    let links = Selector::parse("#idiomas a[href]").expect("static CSS selector");
    let acronyms = Selector::parse("acronym").expect("static CSS selector");
    let base = Url::parse(CVC_BASE).expect("static base URL");
    let mut seen_lng = HashSet::new();
    let mut warnings = Vec::new();

    let links = doc
        .select(&links)
        .filter_map(|element| {
            let href = element.value().attr("href")?;
            let original_url = normalize_cvc_url(href, &base)?;
            let par = original_url
                .query_pairs()
                .find(|(key, _)| key.eq_ignore_ascii_case("Par"))
                .map(|(_, value)| value.into_owned())?;
            if par != expected_par || !par.chars().all(|character| character.is_ascii_digit()) {
                return None;
            }
            let lng = original_url
                .query_pairs()
                .find(|(key, _)| key.eq_ignore_ascii_case("Lng"))
                .map(|(_, value)| value.into_owned())?;
            if lng.is_empty() || !lng.chars().all(|character| character.is_ascii_digit()) {
                return None;
            }
            let Some(acronym_element) = element.select(&acronyms).next() else {
                warnings.push(WaybackImportWarning {
                    par: par.clone(),
                    language: None,
                    reason: format!("missing language acronym for Lng={lng}"),
                });
                return None;
            };
            let acronym = acronym_element.text().collect::<String>();
            let Some(language) = language_code(&acronym) else {
                warnings.push(WaybackImportWarning {
                    par: par.clone(),
                    language: Some(acronym.trim().to_ascii_lowercase()),
                    reason: format!("unknown CVC language acronym {acronym:?}"),
                });
                return None;
            };
            if !seen_lng.insert(lng) {
                return None;
            }
            Some(WaybackLanguageLink {
                language: language.to_string(),
                original_url,
                par,
            })
        })
        .collect();

    LanguageDiscovery { links, warnings }
}

fn language_code(acronym: &str) -> Option<&'static str> {
    match acronym.trim().to_ascii_uppercase().as_str() {
        "ES" => Some("es"),
        "DE" => Some("de"),
        "AR" => Some("ar"),
        "CA" => Some("ca"),
        "FR" => Some("fr"),
        "GL" => Some("gl"),
        "GRC" => Some("grc"),
        "EL" => Some("el"),
        "EN" => Some("en"),
        "IT" => Some("it"),
        "LA" => Some("la"),
        "PL" => Some("pl"),
        "PT" => Some("pt"),
        "RO" => Some("ro"),
        "RU" => Some("ru"),
        "EU" => Some("eu"),
        _ => None,
    }
}

fn parse_wayback_ficha(html: &str) -> Option<String> {
    let doc = Html::parse_document(html);
    let tabs = Selector::parse("#ficha .tabber .tabbertab").expect("static CSS selector");
    let headings = Selector::parse("h2").expect("static CSS selector");
    let paragraphs = Selector::parse("p").expect("static CSS selector");
    let strong = Selector::parse("strong").expect("static CSS selector");

    for tab in doc.select(&tabs) {
        let Some(heading) = tab.select(&headings).next() else {
            continue;
        };
        let heading = heading.text().collect::<String>();
        if normalize(&heading) != "Paremia" {
            continue;
        }
        for paragraph in tab.select(&paragraphs) {
            let Some(label) = paragraph.select(&strong).next() else {
                continue;
            };
            if normalize(&label.text().collect::<String>())
                .trim_end_matches(':')
                .trim()
                != "Enunciado"
            {
                continue;
            }
            let paragraph_text = normalize(&paragraph.text().collect::<String>());
            let Some((_, value)) = paragraph_text.split_once(':') else {
                continue;
            };
            let text = normalize(value);
            if !text.is_empty() {
                return Some(text);
            }
        }
        return None;
    }
    None
}

fn collect_wayback_variants<F>(links: &[WaybackLanguageLink], mut fetch: F) -> VariantCollection
where
    F: FnMut(&WaybackLanguageLink) -> Result<Option<CapturedFicha>>,
{
    let mut collection = VariantCollection {
        entries: Vec::new(),
        available: 0,
        unavailable: 0,
        warnings: Vec::new(),
    };

    for link in links {
        if link.language == "es" {
            continue;
        }
        match fetch(link) {
            Ok(Some(capture)) => {
                collection.available += 1;
                let Some(text) = parse_wayback_ficha(&capture.html) else {
                    collection.warnings.push(WaybackImportWarning {
                        par: link.par.clone(),
                        language: Some(link.language.clone()),
                        reason: "archived ficha has no primary Enunciado".into(),
                    });
                    continue;
                };
                collection.entries.push(NewSaying {
                    text,
                    category: "paremia".into(),
                    language: link.language.clone(),
                    source: "cvc_wayback".into(),
                    source_url: capture.replay_url.to_string(),
                    original_url: Some(link.original_url.to_string()),
                    captured_at: Some(capture.timestamp),
                    group_id: Some(format!("cvc:{}", link.par)),
                });
            }
            Ok(None) => collection.unavailable += 1,
            Err(error) => collection.warnings.push(WaybackImportWarning {
                par: link.par.clone(),
                language: Some(link.language.clone()),
                reason: format!("retrieving archived ficha: {error:#}"),
            }),
        }
    }

    collection
}

fn update_spanish_provenance(entry: &mut NewSaying, original_url: &Url, capture: &CapturedFicha) {
    entry.source_url = capture.replay_url.to_string();
    entry.original_url = Some(original_url.to_string());
    entry.captured_at = Some(capture.timestamp.clone());
}

fn collect_wayback_fichas<F>(roots: Vec<NewSaying>, year: u16, mut fetch: F) -> WaybackImportReport
where
    F: FnMut(&Url, u16) -> Result<Option<CapturedFicha>>,
{
    let roots_processed = roots.len();
    let mut report = WaybackImportReport {
        roots_processed,
        entries: Vec::with_capacity(roots_processed),
        variants_available: 0,
        variants_unavailable: 0,
        captured_spanish_pars: HashSet::new(),
        warnings: Vec::new(),
    };

    for mut entry in roots {
        let par = entry
            .group_id
            .as_deref()
            .and_then(|group_id| group_id.strip_prefix("cvc:"))
            .map(str::to_string);
        let original_url = entry
            .original_url
            .as_deref()
            .and_then(|url| Url::parse(url).ok());
        let (Some(par), Some(original_url)) = (par, original_url) else {
            report.warnings.push(WaybackImportWarning {
                par: entry
                    .group_id
                    .as_deref()
                    .and_then(|group_id| group_id.strip_prefix("cvc:"))
                    .unwrap_or("unknown")
                    .to_string(),
                language: Some("es".into()),
                reason: "Spanish index entry has no valid CVC ficha URL or Par".into(),
            });
            report.entries.push(entry);
            continue;
        };

        match fetch(&original_url, year) {
            Ok(Some(capture)) => {
                update_spanish_provenance(&mut entry, &original_url, &capture);
                report.variants_available += 1;
                report.captured_spanish_pars.insert(par.clone());

                let discovery = parse_wayback_language_links(&capture.html, &par);
                report.warnings.extend(discovery.warnings);
                let variants = collect_wayback_variants(&discovery.links, |link| {
                    fetch(&link.original_url, year)
                });
                report.variants_available += variants.available;
                report.variants_unavailable += variants.unavailable;
                report.warnings.extend(variants.warnings);
                report.entries.push(entry);
                report.entries.extend(variants.entries);
            }
            Ok(None) => {
                report.variants_unavailable += 1;
                report.warnings.push(WaybackImportWarning {
                    par,
                    language: Some("es".into()),
                    reason: "Spanish ficha has no available Wayback capture; kept index entry"
                        .into(),
                });
                report.entries.push(entry);
            }
            Err(error) => {
                report.warnings.push(WaybackImportWarning {
                    par,
                    language: Some("es".into()),
                    reason: format!("retrieving Spanish ficha: {error:#}"),
                });
                report.entries.push(entry);
            }
        }
    }

    report
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

    #[test]
    fn limits_wayback_index_by_unique_par() {
        let html = r#"
            <a href="/lengua/refranero/Ficha.aspx?Lng=0&amp;Par=58807">Haz bien</a>
            <a href="/lengua/refranero/Ficha.aspx?Lng=0&amp;Par=58807">Haz bien</a>
            <a href="/lengua/refranero/Ficha.aspx?Lng=0&amp;Par=58122">A quien madruga</a>
        "#;
        let entries = parse_wayback_index(html, "20240511091123", 2);

        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].group_id.as_deref(), Some("cvc:58807"));
        assert_eq!(entries[1].group_id.as_deref(), Some("cvc:58122"));
    }

    #[test]
    fn prefers_spanish_anchor_and_keeps_first_fallback_for_other_pars() {
        let html = r#"
            <a href="Ficha.aspx?Par=58807&amp;Lng=9">Wrong first language</a>
            <a href="Ficha.aspx?Par=58122&amp;Lng=7">Fallback root entry</a>
            <a href="Ficha.aspx?Par=58807&amp;Lng=0">Spanish root entry</a>
        "#;
        let entries = parse_wayback_index(html, "20240511091123", 2);

        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].group_id.as_deref(), Some("cvc:58807"));
        assert_eq!(entries[0].text, "Spanish root entry");
        assert!(entries[0]
            .original_url
            .as_deref()
            .is_some_and(|url| url.contains("Lng=0")));
        assert_eq!(entries[1].group_id.as_deref(), Some("cvc:58122"));
        assert_eq!(entries[1].text, "Fallback root entry");
        assert!(entries[1]
            .original_url
            .as_deref()
            .is_some_and(|url| url.contains("Lng=7")));
    }

    #[test]
    fn parses_validated_wayback_language_links() {
        let html = r#"
            <div id="idiomas">
              <a href="Ficha.aspx?Par=58807&amp;Lng=9"><acronym>EN</acronym></a>
              <a href="Ficha.aspx?Par=58807&amp;Lng=10"><acronym>CA</acronym></a>
              <a href="Ficha.aspx?Par=58807&amp;Lng=3"><acronym>GRC</acronym></a>
              <a href="Ficha.aspx?Par=58807&amp;Lng=17"><acronym>ZZ</acronym></a>
              <a href="Ficha.aspx?Par=58122&amp;Lng=9"><acronym>EN</acronym></a>
              <a href="https://evil.example/Ficha.aspx?Par=58807&amp;Lng=9">
                <acronym>EN</acronym>
              </a>
            </div>
        "#;
        let discovery = parse_wayback_language_links(html, "58807");

        assert_eq!(
            discovery
                .links
                .iter()
                .map(|link| link.language.as_str())
                .collect::<Vec<_>>(),
            vec!["en", "ca", "grc"]
        );
        assert!(discovery
            .links
            .iter()
            .all(|link| link.original_url.scheme() == "https"));
        let lngs = discovery
            .links
            .iter()
            .map(|link| {
                link.original_url
                    .query_pairs()
                    .find(|(key, _)| key.eq_ignore_ascii_case("Lng"))
                    .map(|(_, value)| value.into_owned())
                    .unwrap()
            })
            .collect::<Vec<_>>();
        assert_eq!(lngs, vec!["9", "10", "3"]);
        assert_eq!(discovery.warnings.len(), 1);
        assert!(discovery.warnings[0].reason.contains("ZZ"));
    }

    #[test]
    fn invalid_acronym_does_not_consume_lng() {
        let html = r#"
            <div id="idiomas">
              <a href="Ficha.aspx?Par=58807&amp;Lng=9"><acronym>ZZ</acronym></a>
              <a href="Ficha.aspx?Par=58807&amp;Lng=9"><acronym>EN</acronym></a>
            </div>
        "#;

        let discovery = parse_wayback_language_links(html, "58807");

        assert_eq!(discovery.links.len(), 1);
        assert_eq!(discovery.links[0].language, "en");
        assert_eq!(discovery.warnings.len(), 1);
        assert!(discovery.warnings[0].reason.contains("ZZ"));
    }

    #[test]
    fn warns_when_language_link_has_no_acronym() {
        let html = r#"
            <div id="idiomas">
              <a href="Ficha.aspx?Par=58807&amp;Lng=15">Idioma sin código</a>
            </div>
        "#;

        let discovery = parse_wayback_language_links(html, "58807");

        assert!(discovery.links.is_empty());
        assert_eq!(discovery.warnings.len(), 1);
        assert_eq!(discovery.warnings[0].par, "58807");
        assert_eq!(discovery.warnings[0].language, None);
        assert!(discovery.warnings[0]
            .reason
            .contains("missing language acronym"));
        assert!(discovery.warnings[0].reason.contains("Lng=15"));
    }

    #[test]
    fn parses_only_the_primary_wayback_enunciado() {
        let html = r#"
            <div id="ficha"><div class="tabber">
              <div class="tabbertab"><h2>Paremia</h2>
                <p><strong>Enunciado: </strong>Haz bien y no mires a quién</p>
              </div>
              <div class="tabbertab"><h2>Variantes</h2>
                <p><strong>Enunciado: </strong>Haz el bien</p>
              </div>
              <div class="tabbertab"><h2>Sinónimos</h2>
                <p>Hacer bien nunca se pierde</p>
              </div>
            </div></div>
        "#;

        assert_eq!(
            parse_wayback_ficha(html).as_deref(),
            Some("Haz bien y no mires a quién")
        );
    }

    #[test]
    fn collects_available_language_records_and_keeps_partial_failures() {
        let links = vec![
            WaybackLanguageLink {
                language: "es".into(),
                original_url: Url::parse(
                    "https://cvc.cervantes.es/lengua/refranero/Ficha.aspx?Par=58807&Lng=0",
                )
                .unwrap(),
                par: "58807".into(),
            },
            WaybackLanguageLink {
                language: "en".into(),
                original_url: Url::parse(
                    "https://cvc.cervantes.es/lengua/refranero/Ficha.aspx?Par=58807&Lng=9",
                )
                .unwrap(),
                par: "58807".into(),
            },
            WaybackLanguageLink {
                language: "ca".into(),
                original_url: Url::parse(
                    "https://cvc.cervantes.es/lengua/refranero/Ficha.aspx?Par=58807&Lng=10",
                )
                .unwrap(),
                par: "58807".into(),
            },
            WaybackLanguageLink {
                language: "fr".into(),
                original_url: Url::parse(
                    "https://cvc.cervantes.es/lengua/refranero/Ficha.aspx?Par=58807&Lng=1",
                )
                .unwrap(),
                par: "58807".into(),
            },
        ];
        let mut processed = Vec::new();
        let report = collect_wayback_variants(&links, |link| {
            processed.push(link.language.clone());
            match link.language.as_str() {
                "en" => Ok(Some(CapturedFicha {
                    html: r#"<div id="ficha"><div class="tabber"><div class="tabbertab">
                        <h2>Paremia</h2><p><strong>Enunciado: </strong>Do good without looking at whom</p>
                    </div></div></div>"#
                        .into(),
                    replay_url: Url::parse(
                        "https://web.archive.org/web/20240511091123/https://cvc.cervantes.es/lengua/refranero/Ficha.aspx?Par=58807&Lng=9",
                    )
                    .unwrap(),
                    timestamp: "20240511091123".into(),
                })),
                "ca" => Err(anyhow!("temporary archive failure")),
                _ => Ok(None),
            }
        });

        assert_eq!(processed, vec!["en", "ca", "fr"]);
        assert_eq!(report.entries.len(), 1);
        assert_eq!(report.available, 1);
        assert_eq!(report.entries[0].language, "en");
        assert_eq!(report.entries[0].source, "cvc_wayback");
        assert_eq!(report.entries[0].category, "paremia");
        assert_eq!(
            report.entries[0].source_url,
            "https://web.archive.org/web/20240511091123/https://cvc.cervantes.es/lengua/refranero/Ficha.aspx?Par=58807&Lng=9"
        );
        assert_eq!(
            report.entries[0].original_url.as_deref(),
            Some("https://cvc.cervantes.es/lengua/refranero/Ficha.aspx?Par=58807&Lng=9")
        );
        assert_eq!(report.entries[0].group_id.as_deref(), Some("cvc:58807"));
        assert_eq!(
            report.entries[0].captured_at.as_deref(),
            Some("20240511091123")
        );
        assert_eq!(report.unavailable, 1);
        assert_eq!(report.warnings.len(), 1);
        assert_eq!(report.warnings[0].par, "58807");
        assert_eq!(report.warnings[0].language.as_deref(), Some("ca"));
    }

    #[test]
    fn updates_spanish_index_provenance_without_adding_a_duplicate() {
        let html = r#"<a href="Ficha.aspx?Par=58807&amp;Lng=0">Haz bien y no mires a quién</a>"#;
        let mut entries = parse_wayback_index(html, "20240511091123", 1);
        let original_url =
            Url::parse("https://cvc.cervantes.es/lengua/refranero/Ficha.aspx?Par=58807&Lng=0")
                .unwrap();
        let capture = CapturedFicha {
            html: String::new(),
            replay_url: Url::parse(
                "https://web.archive.org/web/20240512091123/https://cvc.cervantes.es/lengua/refranero/Ficha.aspx?Par=58807&Lng=0",
            )
            .unwrap(),
            timestamp: "20240512091123".into(),
        };

        update_spanish_provenance(&mut entries[0], &original_url, &capture);

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].text, "Haz bien y no mires a quién");
        assert_eq!(
            entries[0].source_url,
            "https://web.archive.org/web/20240512091123/https://cvc.cervantes.es/lengua/refranero/Ficha.aspx?Par=58807&Lng=0"
        );
        assert_eq!(
            entries[0].original_url.as_deref(),
            Some("https://cvc.cervantes.es/lengua/refranero/Ficha.aspx?Par=58807&Lng=0")
        );
        assert_eq!(entries[0].captured_at.as_deref(), Some("20240512091123"));
    }

    #[test]
    fn validates_wayback_capture_status_host_and_timestamp() {
        let valid = Snapshot {
            available: true,
            url: "http://web.archive.org/web/20240511091123id_/https://cvc.cervantes.es/ficha.aspx"
                .into(),
            timestamp: "20240511091123".into(),
            status: "200".into(),
        };
        let replay = validate_wayback_snapshot(Some(valid)).unwrap().unwrap();
        assert_eq!(replay.replay_url.scheme(), "https");
        assert_eq!(replay.replay_url.host_str(), Some("web.archive.org"));
        assert_eq!(replay.timestamp, "20240511091123");

        let unavailable = Snapshot {
            available: true,
            url: "https://web.archive.org/web/20240511091123/https://cvc.cervantes.es/ficha.aspx"
                .into(),
            timestamp: "20240511091123".into(),
            status: "404".into(),
        };
        assert!(validate_wayback_snapshot(Some(unavailable))
            .unwrap()
            .is_none());
        assert!(validate_wayback_snapshot(None).unwrap().is_none());

        let untrusted = Snapshot {
            available: true,
            url: "https://evil.example/web/20240511091123/https://cvc.cervantes.es/ficha.aspx"
                .into(),
            timestamp: "20240511091123".into(),
            status: "200".into(),
        };
        assert!(validate_wayback_snapshot(Some(untrusted)).is_err());
    }

    #[test]
    fn validates_final_replay_host_after_redirects() {
        let same_host = Url::parse(
            "https://web.archive.org/web/20240511091123/https://cvc.cervantes.es/ficha.aspx",
        )
        .unwrap();
        let external = Url::parse("https://evil.example/ficha.aspx").unwrap();

        assert!(validate_wayback_final_url(&same_host).is_ok());
        assert!(validate_wayback_final_url(&external).is_err());
    }

    #[test]
    fn collects_available_ficha_captures_per_language() {
        let index = r#"<a href="Ficha.aspx?Par=58807&amp;Lng=0">Haz bien y no mires a quién</a>"#;
        let roots = parse_wayback_index(index, "20240511091123", 1);
        let root_html = r#"
            <div id="idiomas">
              <a href="Ficha.aspx?Par=58807&amp;Lng=9"><acronym>EN</acronym></a>
              <a href="Ficha.aspx?Par=58807&amp;Lng=10"><acronym>CA</acronym></a>
              <a href="Ficha.aspx?Par=58807&amp;Lng=1"><acronym>FR</acronym></a>
            </div>
        "#;
        let english_html = r#"<div id="ficha"><div class="tabber">
            <div class="tabbertab"><h2>Paremia</h2>
              <p><strong>Enunciado: </strong>Do good without looking at whom</p>
            </div>
        </div></div>"#;

        let report = collect_wayback_fichas(roots, 2024, |url, _year| {
            let lng = url
                .query_pairs()
                .find(|(key, _)| key.eq_ignore_ascii_case("Lng"))
                .map(|(_, value)| value.into_owned())
                .unwrap();
            let (html, timestamp) = match lng.as_str() {
                "0" => (root_html, "20240512091123"),
                "9" => (english_html, "20240513091123"),
                "10" => return Ok(None),
                "1" => return Err(anyhow!("temporary archive failure")),
                _ => panic!("unexpected Lng={lng}"),
            };
            Ok(Some(CapturedFicha {
                html: html.into(),
                replay_url: Url::parse(&format!(
                    "https://web.archive.org/web/{timestamp}/{}",
                    url.as_str()
                ))
                .unwrap(),
                timestamp: timestamp.into(),
            }))
        });

        assert_eq!(report.roots_processed, 1);
        assert_eq!(report.variants_available, 2);
        assert_eq!(report.variants_unavailable, 1);
        assert!(report.captured_spanish_pars.contains("58807"));
        assert_eq!(report.entries.len(), 2);
        assert_eq!(report.entries[0].language, "es");
        assert_eq!(
            report.entries[0].captured_at.as_deref(),
            Some("20240512091123")
        );
        assert_eq!(report.entries[1].language, "en");
        assert_eq!(
            report.entries[1].captured_at.as_deref(),
            Some("20240513091123")
        );
        assert_eq!(report.entries[0].group_id, report.entries[1].group_id);
        assert_eq!(report.warnings.len(), 1);
        assert_eq!(report.warnings[0].language.as_deref(), Some("fr"));
    }

    #[test]
    fn keeps_index_record_when_spanish_ficha_has_no_capture() {
        let index = r#"<a href="Ficha.aspx?Par=58807&amp;Lng=0">Haz bien y no mires a quién</a>"#;
        let roots = parse_wayback_index(index, "20240511091123", 1);
        let report = collect_wayback_fichas(roots, 2024, |_url, _year| Ok(None));

        assert_eq!(report.entries.len(), 1);
        assert_eq!(report.entries[0].language, "es");
        assert_eq!(
            report.entries[0].captured_at.as_deref(),
            Some("20240511091123")
        );
        assert_eq!(report.variants_unavailable, 1);
        assert_eq!(report.warnings[0].language.as_deref(), Some("es"));
    }
}
