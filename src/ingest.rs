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

#[derive(Debug)]
pub struct UnavailableWaybackIndex;

impl std::fmt::Display for UnavailableWaybackIndex {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("no available Wayback index capture")
    }
}

impl std::error::Error for UnavailableWaybackIndex {}

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
    build_http_client(timeout, false)
}

/// Route Wayback redirects through the same pacing and retry controller.
pub fn wayback_client_with_timeout(timeout: Duration) -> Result<Client> {
    build_http_client(timeout, true)
}

fn build_http_client(timeout: Duration, controlled: bool) -> Result<Client> {
    install_crypto_provider();
    let builder = Client::builder()
        .user_agent("refranero-cli/0.1 (personal opt-in import; contact: local user)")
        .timeout(timeout);
    let builder = if controlled {
        builder
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
    } else {
        builder.redirect(reqwest::redirect::Policy::limited(5))
    };
    Ok(builder.build()?)
}

/// Download a text/html page with an upper body-size limit.
fn fetch_html(client: &Client, url: &str) -> Result<String> {
    let response = client.get(url).send()?.error_for_status()?;
    read_html_response(response)
}

trait WaybackTransport {
    fn get(
        &mut self,
        url: &str,
        activity: &mut dyn FnMut(&str) -> Result<()>,
    ) -> Result<reqwest::blocking::Response>;
}

struct WaybackHttp<'a> {
    client: &'a Client,
    requests: &'a mut crate::wayback_request::WaybackRequests,
}

impl WaybackTransport for WaybackHttp<'_> {
    fn get(
        &mut self,
        url: &str,
        activity: &mut dyn FnMut(&str) -> Result<()>,
    ) -> Result<reqwest::blocking::Response> {
        self.requests.get(self.client, url, activity)
    }
}

fn fetch_wayback_html(
    transport: &mut impl WaybackTransport,
    replay_url: &str,
    activity: &mut impl FnMut(&str) -> Result<()>,
) -> Result<String> {
    let response = transport.get(replay_url, activity)?;
    validate_wayback_final_url(response.url())?;
    let html = read_html_response(response).map_err(mark_wayback_body_error)?;
    validate_wayback_soft_error(&html)?;
    Ok(html)
}

#[derive(Debug)]
struct RetryableWaybackRequest;

impl std::fmt::Display for RetryableWaybackRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("transient Wayback request failure")
    }
}

impl std::error::Error for RetryableWaybackRequest {}

#[cfg(test)]
fn retryable_request_error(message: &str) -> anyhow::Error {
    anyhow!(RetryableWaybackRequest).context(message.to_owned())
}

fn is_retryable_wayback_failure(error: &anyhow::Error) -> bool {
    error
        .chain()
        .any(|cause| cause.is::<RetryableWaybackRequest>())
}

fn is_permanent_variant_http_error(error: &anyhow::Error) -> bool {
    error
        .chain()
        .find_map(|cause| cause.downcast_ref::<reqwest::Error>())
        .and_then(reqwest::Error::status)
        .is_some_and(|status| {
            status.is_client_error()
                && status != reqwest::StatusCode::REQUEST_TIMEOUT
                && status != reqwest::StatusCode::TOO_MANY_REQUESTS
        })
}

fn permanent_variant_http_error_summary(error: &anyhow::Error) -> String {
    let diagnostic = error.chain().find(|cause| {
        let message = cause.to_string();
        message.starts_with("Wayback ") && message.contains(" HTTP ")
    });
    diagnostic.map_or_else(
        || "permanent HTTP error retrieving variant".into(),
        |diagnostic| format!("permanent HTTP error retrieving variant: {diagnostic}"),
    )
}

fn classify_wayback_request_error(error: reqwest::Error) -> anyhow::Error {
    let retryable = error.is_timeout()
        || error.is_connect()
        || error.is_decode()
        || error.status().is_some_and(is_retryable_wayback_status);
    let error = anyhow!(error);
    if retryable {
        error.context(RetryableWaybackRequest)
    } else {
        error
    }
}

fn is_retryable_wayback_status(status: reqwest::StatusCode) -> bool {
    status == reqwest::StatusCode::REQUEST_TIMEOUT
        || status.is_server_error()
        || status == reqwest::StatusCode::TOO_MANY_REQUESTS
}

fn mark_wayback_body_error(error: anyhow::Error) -> anyhow::Error {
    if error.chain().any(|cause| cause.is::<std::io::Error>()) {
        error.context(RetryableWaybackRequest)
    } else {
        error
    }
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
    import_virtual_with_fetch(category, limit, |url| fetch_html(client, url))
}

pub fn import_virtual_with_fetch(
    category: Category,
    limit: usize,
    fetch: impl FnOnce(&str) -> Result<String>,
) -> Result<Vec<NewSaying>> {
    if category == Category::Paremia {
        bail!("use an explicit modern category: refran, proverbio, dicho, adivinanza or cita");
    }
    let url = format!("{VIRTUAL_BASE}{}/", category.url_segment());
    let html = fetch(&url).with_context(|| format!("retrieving {url}"))?;
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
    transport: &mut impl WaybackTransport,
    original_url: &Url,
    year: u16,
    activity: &mut impl FnMut(&str) -> Result<()>,
) -> Result<Option<WaybackReplay>> {
    let timestamp = year.to_string();
    let mut url = Url::parse("https://archive.org/wayback/available")?;
    url.query_pairs_mut()
        .append_pair("url", original_url.as_str())
        .append_pair("timestamp", &timestamp);
    let response = transport.get(url.as_str(), activity)?;
    let response: AvailabilityResponse = response.json().map_err(classify_wayback_request_error)?;
    validate_wayback_snapshot(response.archived_snapshots.closest)
}

fn fetch_wayback_ficha(
    transport: &mut impl WaybackTransport,
    original_url: &Url,
    year: u16,
    activity: &mut impl FnMut(&str) -> Result<()>,
) -> Result<Option<CapturedFicha>> {
    let Some(capture) = find_wayback_capture(transport, original_url, year, activity)? else {
        return Ok(None);
    };
    let replay_url = capture.replay_url;
    let html = fetch_wayback_html(transport, replay_url.as_str(), activity)
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
pub fn import_wayback_stream<P>(
    client: &Client,
    requests: &mut crate::wayback_request::WaybackRequests,
    letter: char,
    year: u16,
    limit: usize,
    events: P,
) -> Result<WaybackImportReport>
where
    P: FnMut(WaybackEvent<'_>) -> Result<()>,
{
    import_wayback_with_transport(
        &mut WaybackHttp { client, requests },
        letter,
        year,
        limit,
        events,
    )
}

fn import_wayback_with_transport<P>(
    transport: &mut impl WaybackTransport,
    letter: char,
    year: u16,
    limit: usize,
    mut events: P,
) -> Result<WaybackImportReport>
where
    P: FnMut(WaybackEvent<'_>) -> Result<()>,
{
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
    let mut index_activity = |activity: &str| {
        events(WaybackEvent::Progress {
            done: 0,
            total: 0,
            activity: &format!("index {activity}"),
        })
    };
    index_activity("index")?;
    let capture = find_wayback_capture(transport, &index_url, year, &mut index_activity)?
        .ok_or_else(|| {
            anyhow!(UnavailableWaybackIndex).context(format!(
                "no available HTTP 200 Wayback capture for {index_url}"
            ))
        })?;
    let html = fetch_wayback_html(transport, capture.replay_url.as_str(), &mut index_activity)
        .with_context(|| format!("retrieving {}", capture.replay_url))?;
    let roots = parse_wayback_index(&html, &capture.timestamp, limit);
    validate_wayback_index_html(&html, !roots.is_empty())?;
    let done = std::cell::Cell::new(0);
    let total = roots.len();
    stream_wayback_fichas(
        roots,
        year,
        |original_url, target_year, events| {
            fetch_wayback_ficha(transport, original_url, target_year, &mut |activity| {
                events(WaybackEvent::Progress {
                    done: done.get(),
                    total,
                    activity,
                })
            })
        },
        &mut |event| {
            if let WaybackEvent::Progress { done: value, .. } = &event {
                done.set(*value);
            }
            events(event)
        },
    )
}

fn validate_wayback_soft_error(html: &str) -> Result<()> {
    let document = Html::parse_document(html);
    let title_selector = Selector::parse("title").expect("static CSS selector");
    let title = document
        .select(&title_selector)
        .next()
        .map(|element| normalize(&element.text().collect::<String>()).to_ascii_lowercase())
        .unwrap_or_default();
    let text = normalize(&document.root_element().text().collect::<String>()).to_ascii_lowercase();
    let error_markers = [
        "wayback machine error",
        "temporarily unavailable",
        "service unavailable",
        "internal server error",
        "too many requests",
        "rate limit",
        "page not found",
        "404 not found",
        "robot check",
    ];
    if error_markers
        .iter()
        .any(|marker| text.contains(marker) || title.contains(marker))
    {
        bail!("Wayback replay returned a soft-error page");
    }
    Ok(())
}

fn validate_wayback_index_html(html: &str, has_roots: bool) -> Result<()> {
    validate_wayback_soft_error(html)?;
    let document = Html::parse_document(html);
    let title_selector = Selector::parse("title").expect("static CSS selector");
    let title = document
        .select(&title_selector)
        .next()
        .map(|element| normalize(&element.text().collect::<String>()).to_ascii_lowercase())
        .unwrap_or_default();
    let text = normalize(&document.root_element().text().collect::<String>()).to_ascii_lowercase();
    if has_roots {
        return Ok(());
    }

    let has_empty_marker = [
        "no hay paremias",
        "no existen paremias",
        "no se encontraron paremias",
        "sin resultados",
        "no results",
    ]
    .iter()
    .any(|marker| text.contains(marker));
    if !title.contains("refranero") || !has_empty_marker {
        bail!("Wayback index replay has no recognized listing or empty-index marker");
    }
    Ok(())
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

/// One skipped or partially failed language ficha during a Wayback import.
pub struct WaybackImportWarning {
    /// CVC `Par` identifier of the ficha.
    pub par: String,
    /// Language code when it was known, `None` when the link had no acronym.
    pub language: Option<String>,
    /// Human-readable explanation, written to stderr by `sync wayback`.
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
    retryable_failures: usize,
}

/// Result of one archived-index import run.
pub struct WaybackImportReport {
    /// Records retrieved, including the Spanish root and its available variants.
    pub entries: Vec<NewSaying>,
    /// Spanish root fichas discovered and processed.
    pub roots_processed: usize,
    /// Linked variants with an HTTP 200 capture.
    pub variants_available: usize,
    /// Linked variants without a usable capture.
    pub variants_unavailable: usize,
    /// Spanish `Par` IDs with a confirmed `Lng=0` capture in this import run.
    pub captured_spanish_pars: HashSet<String>,
    /// Skipped fichas and their reasons.
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
                    par,
                    language: None,
                    reason: format!("missing language acronym for Lng={lng}"),
                });
                return None;
            };
            let acronym = acronym_element.text().collect::<String>();
            let Some(language) = language_code(&acronym) else {
                warnings.push(WaybackImportWarning {
                    par,
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

fn collect_wayback_variants<F, P>(
    links: &[WaybackLanguageLink],
    mut fetch: F,
    on_activity: &mut P,
) -> VariantCollection
where
    F: FnMut(&WaybackLanguageLink) -> Result<Option<CapturedFicha>>,
    P: FnMut(),
{
    let mut collection = VariantCollection {
        entries: Vec::new(),
        available: 0,
        unavailable: 0,
        warnings: Vec::new(),
        retryable_failures: 0,
    };

    for link in links {
        if link.language == "es" {
            continue;
        }
        // Refresh the progress timestamp before each variant so a slow root does not look stalled.
        on_activity();
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
            Err(error) => {
                collection.retryable_failures += usize::from(is_retryable_wayback_failure(&error));
                collection.warnings.push(WaybackImportWarning {
                    par: link.par.clone(),
                    language: Some(link.language.clone()),
                    reason: format!("retrieving archived ficha: {error:#}"),
                });
            }
        }
    }

    collection
}

pub enum WaybackEvent<'a> {
    Progress {
        done: usize,
        total: usize,
        activity: &'a str,
    },
    Entries(&'a [NewSaying], &'a HashSet<String>),
}

fn stream_wayback_fichas<F, P>(
    roots: Vec<NewSaying>,
    year: u16,
    mut fetch: F,
    events: &mut P,
) -> Result<WaybackImportReport>
where
    P: FnMut(WaybackEvent<'_>) -> Result<()>,
    F: FnMut(&Url, u16, &mut P) -> Result<Option<CapturedFicha>>,
{
    let total = roots.len();
    let mut report = WaybackImportReport {
        roots_processed: 0,
        entries: Vec::new(),
        variants_available: 0,
        variants_unavailable: 0,
        captured_spanish_pars: HashSet::new(),
        warnings: Vec::new(),
    };
    for (done, mut entry) in roots.into_iter().enumerate() {
        events(WaybackEvent::Progress {
            done,
            total,
            activity: "root",
        })?;
        let original = Url::parse(entry.original_url.as_deref().context("missing root URL")?)?;
        let par = entry
            .group_id
            .as_deref()
            .and_then(|id| id.strip_prefix("cvc:"))
            .context("missing root Par")?
            .to_owned();
        // The index already provides a usable root. Saving it does not complete the group.
        events(WaybackEvent::Entries(
            std::slice::from_ref(&entry),
            &report.captured_spanish_pars,
        ))?;
        let capture = fetch(&original, year, events)?;
        let links = if let Some(capture) = capture {
            if is_confirmed_spanish_root(&original) {
                update_spanish_provenance(&mut entry, &original, &capture);
                report.captured_spanish_pars.insert(par.clone());
            }
            report.variants_available += 1;
            let discovery = parse_wayback_language_links(&capture.html, &par);
            report.warnings.extend(discovery.warnings);
            discovery.links
        } else {
            report.variants_unavailable += 1;
            report.warnings.push(WaybackImportWarning {
                par,
                language: Some("es".into()),
                reason: "Spanish ficha has no available Wayback capture; kept index entry".into(),
            });
            Vec::new()
        };
        // Keep the root saved even if the first variant request fails.
        events(WaybackEvent::Entries(
            std::slice::from_ref(&entry),
            &report.captured_spanish_pars,
        ))?;
        report.entries.push(entry);
        for link in links.iter().filter(|link| link.language != "es") {
            let mut capture = match fetch(&link.original_url, year, events) {
                Ok(capture) => capture,
                Err(error) if is_permanent_variant_http_error(&error) => {
                    report.warnings.push(WaybackImportWarning {
                        par: link.par.clone(),
                        language: Some(link.language.clone()),
                        reason: permanent_variant_http_error_summary(&error),
                    });
                    continue;
                }
                Err(error) => return Err(error),
            };
            let variants = collect_wayback_variants(
                std::slice::from_ref(link),
                |_| Ok(capture.take()),
                &mut || {},
            );
            events(WaybackEvent::Entries(
                &variants.entries,
                &report.captured_spanish_pars,
            ))?;
            report.entries.extend(variants.entries);
            report.variants_available += variants.available;
            report.variants_unavailable += variants.unavailable;
            report.warnings.extend(variants.warnings);
        }
        report.roots_processed += 1;
        events(WaybackEvent::Progress {
            done: done + 1,
            total,
            activity: "",
        })?;
    }
    Ok(report)
}

fn is_confirmed_spanish_root(url: &Url) -> bool {
    url.query_pairs()
        .any(|(key, value)| key.eq_ignore_ascii_case("Lng") && value == "0")
}

fn update_spanish_provenance(entry: &mut NewSaying, original_url: &Url, capture: &CapturedFicha) {
    entry.source_url = capture.replay_url.to_string();
    entry.original_url = Some(original_url.to_string());
    entry.captured_at = Some(capture.timestamp.clone());
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
    use std::cell::Cell;
    use std::collections::VecDeque;
    use std::rc::Rc;

    struct ImportClock(Rc<Cell<Duration>>);

    impl crate::wayback_request::Clock for ImportClock {
        fn now(&self) -> Duration {
            self.0.get()
        }
        fn wait(&mut self, duration: Duration) {
            self.0.set(self.0.get() + duration);
        }
    }

    struct FakeWayback<F> {
        requests: crate::wayback_request::WaybackRequests<ImportClock>,
        responses: VecDeque<(u16, String, Option<&'static str>)>,
        on_send: F,
    }

    impl<F: FnMut(&str)> WaybackTransport for FakeWayback<F> {
        fn get(
            &mut self,
            url: &str,
            activity: &mut dyn FnMut(&str) -> Result<()>,
        ) -> Result<reqwest::blocking::Response> {
            use reqwest::ResponseBuilderExt;
            self.requests.get_with(
                url,
                |url| {
                    (self.on_send)(url);
                    let (status, body, retry_after) = self
                        .responses
                        .pop_front()
                        .expect("solicitud HTTP no prevista");
                    let mut response = http::Response::builder()
                        .status(status)
                        .url(Url::parse(url).unwrap());
                    if let Some(value) = retry_after {
                        response = response.header("retry-after", value);
                    }
                    Ok(response.body(body).unwrap().into())
                },
                activity,
            )
        }
    }

    fn available_capture(original: &str) -> String {
        serde_json::json!({"archived_snapshots": {"closest": {
            "available": true, "status": "200", "timestamp": "20240511091123",
            "url": format!("https://web.archive.org/web/20240511091123/{original}")
        }}})
        .to_string()
    }

    #[test]
    fn import_http_events_persist_progress_and_translation_before_rate_limit_abort() {
        let mut store = crate::store::Store::open(std::path::Path::new(":memory:")).unwrap();
        store
            .try_acquire_refresh_lease(crate::unix_timestamp(), "test")
            .unwrap();
        let now = Rc::new(Cell::new(Duration::ZERO));
        let mut labels = Vec::new();
        let mut sent_at = Vec::new();
        let mut letters = Vec::new();
        let result = crate::update::run_wayback_updates(
            &mut store,
            |store, letter, _, _| {
                letters.push(letter);
                let phase = crate::wayback_phase(letter, true);
                let mut transport = FakeWayback {
                requests: crate::wayback_request::WaybackRequests::with_clock(ImportClock(now.clone())),
                responses: VecDeque::from([
                    (200, available_capture("https://cvc.cervantes.es/lengua/refranero/listado.aspx?letra=A"), None),
                    (200, r#"<a href="Ficha.aspx?Par=1&Lng=0">Primera</a><a href="Ficha.aspx?Par=2&Lng=0">Segunda</a>"#.into(), None),
                    (200, available_capture("https://cvc.cervantes.es/lengua/refranero/Ficha.aspx?Par=1&Lng=0"), None),
                    (200, r#"<div id="idiomas"><a href="Ficha.aspx?Par=1&Lng=9"><acronym>EN</acronym></a></div>"#.into(), None),
                    (200, available_capture("https://cvc.cervantes.es/lengua/refranero/Ficha.aspx?Par=1&Lng=9"), None),
                    (200, r#"<div id="ficha"><div class="tabber"><div class="tabbertab"><h2>Paremia</h2><p><strong>Enunciado: </strong>First</p></div></div></div>"#.into(), None),
                    (429, String::new(), Some("7")),
                    (429, String::new(), Some("7")),
                    (429, String::new(), Some("7")),
                ]),
                on_send: |url: &str| {
                    sent_at.push(now.get());
                    let parsed = Url::parse(url).unwrap();
                    if parsed.query_pairs().any(|(key, value)| key == "url" && value.contains("Par=2")) {
                        assert_eq!(store.count_source("cvc_wayback").unwrap(), 3, "previous root and variant are already persisted");
                        let progress = store.refresh_progress().unwrap().unwrap();
                        assert_eq!((progress.done, progress.total), (1, 2));
                    }
                },
            };
                let result =
                    import_wayback_with_transport(&mut transport, letter, 2024, 2, |event| {
                        crate::handle_wayback_event(store, "test", &phase, event)?;
                        if let Some(progress) = store.refresh_progress()? {
                            labels.push(
                                crate::presentation::Catalog::new(
                                    crate::presentation::UiLanguage::Spanish,
                                )
                                .progress_phase(&progress.phase),
                            );
                        }
                        Ok(())
                    });
                assert!(transport.responses.is_empty());
                result.map(|report| report.entries.len())
            },
            |_, _, _, _| {},
        );
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("retries exhausted"));
        assert_eq!(letters, vec!['A']);
        assert_eq!(store.wayback_bootstrap_next().unwrap(), Some('A'));
        assert_eq!(store.count_source("cvc_wayback").unwrap(), 3);
        assert!(store
            .random("en", Some("paremia"), 50)
            .unwrap()
            .iter()
            .any(|saying| saying.entry.source == "cvc_wayback" && saying.entry.text == "First"));
        assert_eq!(sent_at.len(), 9);
        assert!(sent_at[7] - sent_at[6] >= Duration::from_secs(7));
        assert!(sent_at[8] - sent_at[7] >= Duration::from_secs(7));
        assert!(labels
            .iter()
            .any(|label| label.contains("índice solicitud")));
        assert!(labels
            .iter()
            .any(|label| label.contains("espera/reintento 7s")));
    }

    #[test]
    fn streaming_preserves_index_root_when_its_capture_request_fails() {
        let store = crate::store::Store::open(std::path::Path::new(":memory:")).unwrap();
        let roots = super::parse_wayback_index(
            r#"<a href="Ficha.aspx?Par=1&Lng=0">Primera</a>"#,
            "20240511091123",
            1,
        );
        let mut completed = 0;
        let result = super::stream_wayback_fichas(
            roots,
            2024,
            |_, _, _| Err(super::retryable_request_error("429")),
            &mut |event| {
                match event {
                    super::WaybackEvent::Entries(entries, pars) => {
                        store.insert_wayback(entries, pars)?;
                    }
                    super::WaybackEvent::Progress { done, .. } => completed = done,
                }
                Ok(())
            },
        );
        assert!(result.is_err());
        assert_eq!(
            store.count_source("cvc_wayback").unwrap(),
            1,
            "la raíz del índice ya estaba disponible"
        );
        assert_eq!(completed, 0, "el grupo no ha terminado");
    }

    #[test]
    fn sqlite_write_failures_and_lost_lease_stop_streaming_requests() {
        for failure in ["progress", "entries", "lease"] {
            let path = std::env::temp_dir().join(format!(
                "refranero-wayback-{failure}-{}-{}.sqlite3",
                std::process::id(),
                crate::unix_timestamp()
            ));
            let store = crate::store::Store::open(&path).unwrap();
            store
                .try_acquire_refresh_lease(crate::unix_timestamp(), "test")
                .unwrap();
            let connection = rusqlite::Connection::open(&path).unwrap();
            match failure {
                "progress" => connection.execute_batch("CREATE TRIGGER fail_progress BEFORE INSERT ON metadata WHEN NEW.key = 'refresh_progress' BEGIN SELECT RAISE(FAIL, 'progress write failed'); END;").unwrap(),
                "entries" => connection.execute_batch("CREATE TRIGGER fail_entries BEFORE INSERT ON sayings BEGIN SELECT RAISE(FAIL, 'entry write failed'); END;").unwrap(),
                _ => {},
            }
            let roots = super::parse_wayback_index(
                r#"<a href="Ficha.aspx?Par=1&Lng=0">Primera</a><a href="Ficha.aspx?Par=2&Lng=0">Segunda</a>"#,
                "20240511091123",
                2,
            );
            let mut calls = 0;
            let owner = if failure == "lease" {
                "wrong owner"
            } else {
                "test"
            };
            let result = super::stream_wayback_fichas(
                roots,
                2024,
                |_, _, _| {
                    calls += 1;
                    Ok(None)
                },
                &mut |event| {
                    crate::handle_wayback_event(&store, owner, "Wayback bootstrap 1/26 (A)", event)
                },
            );
            assert!(result.is_err(), "{failure}");
            assert_eq!(calls, 0, "{failure}: cancelar solicitudes siguientes");
            assert_eq!(store.wayback_bootstrap_next().unwrap(), Some('A'));
            assert_eq!(store.count_source("cvc_wayback").unwrap(), 0);
            drop(connection);
            drop(store);
            for suffix in ["", "-wal", "-shm"] {
                let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
            }
        }
    }

    #[test]
    fn streaming_keeps_root_before_variant_failure_and_cancels_writes() {
        let root_html =
            r#"<div id="idiomas"><a href="Ficha.aspx?Par=1&Lng=9"><acronym>EN</acronym></a></div>"#;
        let roots = super::parse_wayback_index(
            r#"<a href="Ficha.aspx?Par=1&Lng=0">Primera</a><a href="Ficha.aspx?Par=2&Lng=0">Segunda</a>"#,
            "20240511091123",
            2,
        );
        let store = crate::store::Store::open(std::path::Path::new(":memory:")).unwrap();
        let mut calls = 0;
        let result = super::stream_wayback_fichas(
            roots.clone(),
            2024,
            |url, _, _| {
                calls += 1;
                if calls == 2 {
                    return Err(super::retryable_request_error("variant 429"));
                }
                Ok(Some(super::CapturedFicha {
                    html: root_html.into(),
                    replay_url: url.clone(),
                    timestamp: "20240511091123".into(),
                }))
            },
            &mut |event| {
                if let super::WaybackEvent::Entries(entries, pars) = event {
                    store.insert_wayback(entries, pars)?;
                }
                Ok(())
            },
        );
        assert!(result.is_err());
        assert_eq!(calls, 2, "must not request the second root");
        assert_eq!(store.count_source("cvc_wayback").unwrap(), 1);

        for fail_on_progress in [false, true] {
            let mut calls = 0;
            let result = super::stream_wayback_fichas(
                roots.clone(),
                2024,
                |_, _, _| {
                    calls += 1;
                    Ok(None)
                },
                &mut |event| {
                    if matches!(event, super::WaybackEvent::Progress { .. }) == fail_on_progress {
                        anyhow::bail!("lease or persistence error");
                    }
                    Ok(())
                },
            );
            assert!(result.is_err());
            assert_eq!(calls, 0);
        }
    }

    #[test]
    fn streaming_saves_first_group_and_progress_before_second_root_failure() {
        use std::cell::Cell;
        let store = crate::store::Store::open(std::path::Path::new(":memory:")).unwrap();
        let roots = super::parse_wayback_index(
            r#"<a href="Ficha.aspx?Par=1&Lng=0">Primera</a><a href="Ficha.aspx?Par=2&Lng=0">Segunda</a>"#,
            "20240511091123",
            2,
        );
        let done = Cell::new(0);
        let mut calls = 0;
        let result = super::stream_wayback_fichas(
            roots.clone(),
            2024,
            |_, _, _| {
                calls += 1;
                if calls == 2 {
                    assert_eq!(
                        store.count_source("cvc_wayback").unwrap(),
                        2,
                        "first group and next index root are persisted before the second request"
                    );
                    assert_eq!(done.get(), 1, "real progress within A");
                    return Err(super::retryable_request_error("429 retry budget exhausted"));
                }
                Ok(None)
            },
            &mut |event| {
                match event {
                    super::WaybackEvent::Entries(entries, pars) => {
                        store.insert_wayback(entries, pars)?;
                    }
                    super::WaybackEvent::Progress { done: value, .. } => done.set(value),
                }
                Ok(())
            },
        );
        assert!(result.is_err());
        assert_eq!(calls, 2);
        assert_eq!(store.count_source("cvc_wayback").unwrap(), 2);
        super::stream_wayback_fichas(roots, 2024, |_, _, _| Ok(None), &mut |event| {
            if let super::WaybackEvent::Entries(entries, pars) = event {
                store.insert_wayback(entries, pars)?;
            }
            Ok(())
        })
        .unwrap();
        assert_eq!(
            store.count_source("cvc_wayback").unwrap(),
            2,
            "resume is idempotent"
        );
    }

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
    fn parses_every_virtual_record_when_the_caller_requests_an_unbounded_page() {
        let html = (0..55)
            .map(|index| format!("<blockquote>Refrán número {index}.</blockquote>"))
            .collect::<String>();
        let category_url = "https://www.cervantesvirtual.com/portales/refranes_dichos_proverbios_citas_adivinanzas/refran/";

        let entries = parse_virtual(&html, Category::Refran, category_url, usize::MAX);

        assert_eq!(entries.len(), 55);
        assert_eq!(entries.last().unwrap().text, "Refrán número 54.");
    }

    #[test]
    fn soft_error_validator_applies_to_replay_bodies() {
        for body in [
            "<title>Wayback Machine Error</title>",
            "Service unavailable",
            "Robot check",
        ] {
            assert!(validate_wayback_soft_error(body).is_err(), "{body}");
        }
        assert!(
            validate_wayback_soft_error("<title>Refranero</title><p>Contenido válido</p>").is_ok()
        );
    }

    #[test]
    fn fallback_lng_seven_root_capture_does_not_confirm_spanish_provenance() {
        let roots = parse_wayback_index(
            r#"<a href="Ficha.aspx?Par=17&amp;Lng=7">Fallback root</a>"#,
            "20240511091123",
            1,
        );
        let original = roots[0].original_url.clone().unwrap();
        let replay = Url::parse("https://web.archive.org/web/20240511091123/https://cvc.cervantes.es/lengua/refranero/Ficha.aspx?Par=17&Lng=7").unwrap();
        let mut stored = Vec::new();
        let report = stream_wayback_fichas(
            roots,
            2024,
            |url, _, _| {
                assert_eq!(url.as_str(), original);
                Ok(Some(CapturedFicha {
                    html: "<div id='idiomas'></div>".into(),
                    replay_url: replay.clone(),
                    timestamp: "20240511091123".into(),
                }))
            },
            &mut |event| {
                if let WaybackEvent::Entries(entries, pars) = event {
                    stored.extend_from_slice(entries);
                    assert!(pars.is_empty());
                }
                Ok(())
            },
        )
        .unwrap();
        assert!(report.captured_spanish_pars.is_empty());
        assert_eq!(stored.last().unwrap().source_url, "https://web.archive.org/web/20240511091123/https://cvc.cervantes.es/lengua/refranero/Ficha.aspx?Par=17&Lng=7");
    }

    #[test]
    fn permanent_variant_http_error_skips_variant_and_continues_later_root() {
        let roots = parse_wayback_index(
            r#"<a href="Ficha.aspx?Par=1&amp;Lng=0">First</a><a href="Ficha.aspx?Par=2&amp;Lng=0">Second</a>"#,
            "20240511091123",
            2,
        );
        let first_root = r#"<div id="idiomas"><a href="Ficha.aspx?Par=1&amp;Lng=9"><acronym>EN</acronym></a></div>"#;
        let replay = |url: &Url, html: &str| CapturedFicha {
            html: html.into(),
            replay_url: url.clone(),
            timestamp: "20240511091123".into(),
        };
        let calls = Cell::new(0);
        let report = stream_wayback_fichas(
            roots,
            2024,
            |url, _, _| {
                let call = calls.get() + 1;
                calls.set(call);
                match call {
                    1 => Ok(Some(replay(url, first_root))),
                    2 => {
                        use reqwest::ResponseBuilderExt;
                        let response: reqwest::blocking::Response = http::Response::builder()
                            .status(404)
                            .url(url.clone())
                            .body(String::new())
                            .unwrap()
                            .into();
                        Err(anyhow!(response.error_for_status().unwrap_err()))
                    }
                    3 => Ok(Some(replay(url, "<div id='idiomas'></div>"))),
                    _ => panic!("unexpected request"),
                }
            },
            &mut |_| Ok(()),
        )
        .unwrap();

        assert_eq!(calls.get(), 3);
        assert_eq!(report.roots_processed, 2);
        assert_eq!(report.warnings.len(), 1);
        assert_eq!(report.warnings[0].language.as_deref(), Some("en"));
    }

    #[test]
    fn http_499_variant_capture_warns_once_and_continues_import() {
        let variant_capture_attempts = Cell::new(0);
        let mut transport = FakeWayback {
            requests: crate::wayback_request::WaybackRequests::with_clock(ImportClock(
                Rc::new(Cell::new(Duration::ZERO)),
            )),
            responses: VecDeque::from([
                (
                    200,
                    available_capture("https://cvc.cervantes.es/lengua/refranero/listado.aspx?letra=A"),
                    None,
                ),
                (
                    200,
                    r#"<a href="Ficha.aspx?Par=1&amp;Lng=0">First</a><a href="Ficha.aspx?Par=2&amp;Lng=0">Second</a>"#.into(),
                    None,
                ),
                (
                    200,
                    available_capture("https://cvc.cervantes.es/lengua/refranero/Ficha.aspx?Par=1&Lng=0"),
                    None,
                ),
                (
                    200,
                    r#"<div id="idiomas"><a href="Ficha.aspx?Par=1&amp;Lng=9"><acronym>EN</acronym></a></div>"#.into(),
                    None,
                ),
                (
                    200,
                    available_capture("https://cvc.cervantes.es/lengua/refranero/Ficha.aspx?Par=1&Lng=9"),
                    None,
                ),
                (499, "private response body".into(), None),
                (
                    200,
                    available_capture("https://cvc.cervantes.es/lengua/refranero/Ficha.aspx?Par=2&Lng=0"),
                    None,
                ),
                (
                    200,
                    r#"<div id="idiomas"></div>"#.into(),
                    None,
                ),
            ]),
            on_send: |url: &str| {
                let parsed = Url::parse(url).unwrap();
                if parsed.path().starts_with("/web/")
                    && parsed.query_pairs().any(|(key, value)| {
                        key == "Par" && value == "1"
                    })
                    && parsed.query_pairs().any(|(key, value)| {
                        key == "Lng" && value == "9"
                    })
                {
                    variant_capture_attempts.set(variant_capture_attempts.get() + 1);
                }
            },
        };

        let report = import_wayback_with_transport(&mut transport, 'A', 2024, 2, |_| Ok(()))
            .expect("a permanent variant error must not abort the import");

        assert!(transport.responses.is_empty());
        assert_eq!(variant_capture_attempts.get(), 1);
        assert_eq!(report.roots_processed, 2);
        assert_eq!(report.warnings.len(), 1);
        assert_eq!(report.warnings[0].language.as_deref(), Some("en"));
        assert!(report.warnings[0].reason.contains("HTTP 499"));
        assert!(report.warnings[0].reason.contains("archived capture"));
        assert!(!report.warnings[0].reason.contains("private response body"));
        assert!(!report.warnings[0].reason.contains("Par=1"));
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
    fn index_navigation_without_empty_marker_keeps_a_pending() {
        let html = r#"<html><head><title>Refranero multilingüe</title></head>
            <body><a href="listado.aspx?letra=A">A</a><section>Contenido desconocido</section></body></html>"#;
        let mut store = crate::store::Store::open(std::path::Path::new(":memory:")).unwrap();
        let mut letters = Vec::new();
        let result = crate::update::run_wayback_updates(
            &mut store,
            |_, letter, _, _| {
                letters.push(letter);
                let mut transport = FakeWayback {
                    requests: crate::wayback_request::WaybackRequests::with_clock(ImportClock(
                        Rc::new(Cell::new(Duration::ZERO)),
                    )),
                    responses: VecDeque::from([
                        (
                            200,
                            available_capture(
                                "https://cvc.cervantes.es/lengua/refranero/listado.aspx?letra=A",
                            ),
                            None,
                        ),
                        (200, html.into(), None),
                    ]),
                    on_send: |_: &str| {},
                };
                import_wayback_with_transport(&mut transport, letter, 2024, 2, |_| Ok(()))
                    .map(|report| report.entries.len())
            },
            |_, _, _, _| {},
        );
        assert!(
            result.is_err(),
            "navegación intacta no demuestra que el índice esté vacío"
        );
        assert_eq!(letters, vec!['A']);
        assert_eq!(store.wayback_bootstrap_next().unwrap(), Some('A'));
        assert_eq!(store.wayback_cursor().unwrap(), None);
    }

    #[test]
    fn accepts_a_successfully_fetched_index_with_no_fichas_as_empty() {
        let html = r#"<!doctype html><html><head><title>Refranero multilingüe</title></head>
            <body><a href="listado.aspx?letra=A">A</a>
            <p>No hay paremias para esta letra.</p></body></html>"#;

        assert!(validate_wayback_index_html(html, false).is_ok());
        let entries = parse_wayback_index(html, "20240511091123", usize::MAX);

        assert!(entries.is_empty());
        let mut transport = FakeWayback {
            requests: crate::wayback_request::WaybackRequests::with_clock(ImportClock(Rc::new(
                Cell::new(Duration::ZERO),
            ))),
            responses: VecDeque::from([
                (
                    200,
                    available_capture(
                        "https://cvc.cervantes.es/lengua/refranero/listado.aspx?letra=A",
                    ),
                    None,
                ),
                (200, html.into(), None),
            ]),
            on_send: |_: &str| {},
        };
        let report =
            import_wayback_with_transport(&mut transport, 'A', 2024, 2, |_| Ok(())).unwrap();
        assert_eq!(report.roots_processed, 0);
        assert!(report.entries.is_empty());
    }

    #[test]
    fn rejects_soft_error_pages_instead_of_treating_them_as_empty_indexes() {
        let html = r#"<!doctype html><html><head><title>Wayback Machine Error</title></head>
            <body><h1>Sorry, this page is temporarily unavailable</h1></body></html>"#;

        assert!(validate_wayback_index_html(html, false).is_err());
    }

    #[test]
    fn rejects_unrecognized_cvc_markup_without_records_or_an_empty_marker() {
        let html = r#"<!doctype html><html><head><title>Refranero</title></head>
            <body><main><p>La estructura de la página ha cambiado.</p></main></body></html>"#;

        assert!(validate_wayback_index_html(html, false).is_err());
    }

    #[test]
    fn rejects_unrelated_listing_navigation_as_an_empty_cvc_index() {
        let html = r#"<!doctype html><html><head><title>Refranero</title></head>
            <body><a href="https://unrelated.example/listado.aspx?letra=A">A</a>
            <p>Contenido sin índice.</p></body></html>"#;

        assert!(validate_wayback_index_html(html, false).is_err());
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
        let mut activity = 0;
        let report = collect_wayback_variants(
            &links,
            |link| {
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
                "ca" => Err(retryable_request_error("temporary archive failure")),
                _ => Ok(None),
            }
            },
            &mut || activity += 1,
        );

        assert_eq!(processed, vec!["en", "ca", "fr"]);
        assert_eq!(activity, 3, "progress ticks before each variant fetch");
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
        assert_eq!(report.retryable_failures, 1);
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
    fn only_retryable_http_statuses_mark_wayback_request_failures() {
        assert!(is_retryable_wayback_status(
            reqwest::StatusCode::TOO_MANY_REQUESTS
        ));
        assert!(is_retryable_wayback_status(
            reqwest::StatusCode::SERVICE_UNAVAILABLE
        ));
        assert!(is_retryable_wayback_status(
            reqwest::StatusCode::REQUEST_TIMEOUT
        ));
        assert!(!is_retryable_wayback_status(reqwest::StatusCode::NOT_FOUND));
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

        let report = stream_wayback_fichas(
            roots,
            2024,
            |url, _year, _events| {
                let lng = url
                    .query_pairs()
                    .find(|(key, _)| key.eq_ignore_ascii_case("Lng"))
                    .map(|(_, value)| value.into_owned())
                    .unwrap();
                let (html, timestamp) = match lng.as_str() {
                    "0" => (root_html, "20240512091123"),
                    "9" => (english_html, "20240513091123"),
                    "10" => return Ok(None),
                    "1" => (root_html, "20240514091123"),
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
            },
            &mut |_| Ok(()),
        )
        .unwrap();

        assert_eq!(report.roots_processed, 1);
        assert_eq!(report.variants_available, 3);
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
    fn parse_and_language_warnings_are_not_retryable_request_failures() {
        let links = vec![WaybackLanguageLink {
            language: "en".into(),
            original_url: Url::parse(
                "https://cvc.cervantes.es/lengua/refranero/Ficha.aspx?Par=58807&Lng=9",
            )
            .unwrap(),
            par: "58807".into(),
        }];

        let report = collect_wayback_variants(
            &links,
            |_| {
                Ok(Some(CapturedFicha {
                    html: "<html><body>Ficha sin Enunciado</body></html>".into(),
                    replay_url: Url::parse(
                        "https://web.archive.org/web/20240511091123/https://cvc.cervantes.es/lengua/refranero/Ficha.aspx?Par=58807&Lng=9",
                    )
                    .unwrap(),
                    timestamp: "20240511091123".into(),
                }))
            },
            &mut || {},
        );

        assert_eq!(report.retryable_failures, 0);
        assert_eq!(report.warnings.len(), 1);
        assert!(report.warnings[0].reason.contains("no primary Enunciado"));
    }

    #[test]
    fn keeps_index_record_when_spanish_ficha_has_no_capture() {
        let index = r#"<a href="Ficha.aspx?Par=58807&amp;Lng=0">Haz bien y no mires a quién</a>"#;
        let roots = parse_wayback_index(index, "20240511091123", 1);
        let report = stream_wayback_fichas(
            roots,
            2024,
            |_url, _year, _events| Ok(None),
            &mut |_| Ok(()),
        )
        .unwrap();

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
