//! Interactive reading session: terminal lifecycle, key handling and state transitions.
//!
//! The session owns I/O and catalog access. Composition stays in [`crate::presentation`], so the
//! layout keeps its snapshot coverage.
use std::io::{self, Write};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use crossterm::cursor::{Hide, MoveTo, Show};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::style::Print;
use crossterm::terminal::{self, Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::{execute, queue};

use crate::domain::{Category, Saying};
use crate::presentation::{self, compose, ColorMode, ProgressView, Screen};
use crate::store::Store;

const DEFAULT_CATEGORY: &str = "refran";
/// How often the reader checks for a catalog refresh while waiting for keys.
const REFRESH_POLL: Duration = Duration::from_secs(2);
/// Progress older than this is ignored, so a dead worker cannot leave a stuck bar.
const PROGRESS_TTL_SECONDS: i64 = 60;
/// Categories the reader can cycle through, including `paremia` from Wayback imports.
const CATEGORY_CYCLE: [&str; 6] = [
    "refran",
    "paremia",
    "proverbio",
    "dicho",
    "adivinanza",
    "cita",
];

/// One reading session over the local catalog.
pub struct Session<'a> {
    store: &'a Store,
    category: Option<String>,
    current: Saying,
    variants: Vec<Saying>,
    variant_index: usize,
    message: Option<String>,
    help: bool,
    observed_refresh: Option<i64>,
    observed_wayback: Option<i64>,
    progress: Option<ProgressView>,
    dirty: bool,
    color: bool,
    links: bool,
}

impl<'a> Session<'a> {
    /// Load a first random entry and its multilingual variants.
    pub fn start(
        store: &'a Store,
        language: &str,
        category: Option<Category>,
        color_mode: ColorMode,
    ) -> Result<Self> {
        let category = category
            .map(|value| value.as_str().to_owned())
            .unwrap_or_else(|| DEFAULT_CATEGORY.to_owned());
        let current = store
            .random(language, Some(&category), 1)?
            .into_iter()
            .next()
            .with_context(|| format!("no local entries for lang={language} category={category}"))?;
        Self::with_current(store, current, Some(category), color_mode)
    }

    fn with_current(
        store: &'a Store,
        current: Saying,
        category: Option<String>,
        color_mode: ColorMode,
    ) -> Result<Self> {
        let options = presentation::detect(color_mode);
        let observed_refresh = store.last_successful_refresh()?;
        let observed_wayback = store.last_wayback_refresh()?;
        let mut session = Self {
            store,
            category,
            current,
            variants: Vec::new(),
            variant_index: 0,
            message: None,
            help: false,
            observed_refresh,
            observed_wayback,
            progress: None,
            dirty: true,
            color: options.color,
            links: options.links,
        };
        session.reload_variants()?;
        Ok(session)
    }

    /// Run the event loop until the reader quits.
    pub fn run(&mut self) -> Result<()> {
        let mut terminal = TerminalGuard::enter()?;
        let mut stdout = io::stdout();
        let mut next_poll = Instant::now() + REFRESH_POLL;
        loop {
            if self.dirty {
                let (width, height) = terminal::size().unwrap_or((80, 24));
                self.draw(&mut stdout, width, height)?;
                self.dirty = false;
            }
            let now = Instant::now();
            if now >= next_poll {
                self.poll_refresh();
                next_poll = Instant::now() + REFRESH_POLL;
                continue;
            }
            let wait = next_poll.saturating_duration_since(now).min(REFRESH_POLL);
            if !event::poll(wait)? {
                continue;
            }
            match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    if !self.handle(key.code, key.modifiers)? {
                        break;
                    }
                }
                Event::Resize(_, _) => {}
                _ => {}
            }
            self.dirty = true;
        }
        terminal.leave()
    }

    fn handle(&mut self, code: KeyCode, modifiers: KeyModifiers) -> Result<bool> {
        if modifiers.contains(KeyModifiers::CONTROL) && code == KeyCode::Char('c') {
            return Ok(false);
        }
        match code {
            KeyCode::Char('q') | KeyCode::Esc => return Ok(false),
            KeyCode::Char('?') => {
                self.help = !self.help;
                return Ok(true);
            }
            _ if self.help => {
                self.help = false;
                return Ok(true);
            }
            KeyCode::Char('n') | KeyCode::Enter | KeyCode::Char(' ') => self.next_saying()?,
            KeyCode::Char('l') | KeyCode::Right => self.cycle_language(1)?,
            KeyCode::Char('h') | KeyCode::Left => self.cycle_language(-1)?,
            KeyCode::Char('c') => self.cycle_category()?,
            KeyCode::Char('o') => self.open_source(),
            _ => {}
        }
        Ok(true)
    }

    fn draw(&self, stdout: &mut io::Stdout, width: u16, height: u16) -> Result<()> {
        let (languages_label, languages) = self.language_strip();
        let frame = compose(&Screen {
            saying: &self.current,
            next_language: self.next_language(),
            message: self.message.clone(),
            help: self.help,
            width: usize::from(width),
            height: usize::from(height),
            languages,
            languages_label,
            progress: self.progress.clone(),
            color: self.color,
            links: self.links,
        });
        queue!(stdout, MoveTo(0, 0), Clear(ClearType::All))?;
        for (index, line) in frame.lines().enumerate() {
            queue!(stdout, MoveTo(0, index as u16), Print(line))?;
        }
        stdout.flush()?;
        Ok(())
    }

    /// Refresh the status bar from the background worker without blocking the keys.
    fn poll_refresh(&mut self) {
        self.poll_progress();
        self.poll_generation();
    }

    fn poll_progress(&mut self) {
        let Ok(stored) = self.store.refresh_progress() else {
            return;
        };
        let fresh = stored
            .filter(|progress| {
                unix_now().saturating_sub(progress.updated_at) <= PROGRESS_TTL_SECONDS
            })
            .map(|progress| ProgressView {
                label: format!("Actualizando {}", progress.phase),
                done: progress.done,
                total: progress.total,
            });
        if fresh != self.progress {
            self.progress = fresh;
            self.dirty = true;
        }
    }

    fn poll_generation(&mut self) {
        let (Ok(virtual_latest), Ok(wayback_latest)) = (
            self.store.last_successful_refresh(),
            self.store.last_wayback_refresh(),
        ) else {
            return;
        };
        if virtual_latest == self.observed_refresh && wayback_latest == self.observed_wayback {
            return;
        }
        // Keep the old generation when the reload fails so the next poll retries it.
        if self.reload_variants().is_err() {
            return;
        }
        self.observed_refresh = virtual_latest;
        self.observed_wayback = wayback_latest;
        self.message = Some("Catálogo actualizado".to_owned());
        self.dirty = true;
    }

    fn language_strip(&self) -> (Option<String>, Vec<String>) {
        if self.variants.len() > 1 {
            let mut codes: Vec<String> = self
                .variants
                .iter()
                .map(|saying| saying.entry.language.clone())
                .collect();
            spanish_first(&mut codes);
            (Some("Otros idiomas".to_owned()), codes)
        } else {
            // A single-language entry still shows its language: nothing to hide.
            (
                Some("Idioma".to_owned()),
                vec![self.current.entry.language.clone()],
            )
        }
    }

    fn next_language(&self) -> Option<String> {
        if self.variants.len() < 2 {
            return None;
        }
        let next = (self.variant_index + 1) % self.variants.len();
        Some(self.variants[next].entry.language.clone())
    }

    fn reload_variants(&mut self) -> Result<()> {
        self.variants = match self.current.entry.group_id.as_deref() {
            Some(group) => self.store.by_group(group)?,
            None => Vec::new(),
        };
        self.variant_index = self
            .variants
            .iter()
            .position(|saying| saying.id == self.current.id)
            .unwrap_or(0);
        Ok(())
    }

    fn next_saying(&mut self) -> Result<()> {
        let language = self.current.entry.language.clone();
        let next = self.store.random(&language, self.category.as_deref(), 1)?;
        if let Some(next) = next.into_iter().next() {
            self.current = next;
            self.message = None;
            self.reload_variants()?;
        } else {
            self.message = Some("No hay más refranes con este filtro".to_owned());
        }
        Ok(())
    }

    fn cycle_language(&mut self, direction: isize) -> Result<()> {
        if self.variants.len() < 2 {
            self.message = Some(format!(
                "Este refrán solo está en {}",
                presentation::language_name(&self.current.entry.language)
            ));
            return Ok(());
        }
        let len = self.variants.len() as isize;
        self.variant_index = (self.variant_index as isize + direction).rem_euclid(len) as usize;
        self.current = self.variants[self.variant_index].clone();
        self.message = Some(format!(
            "Idioma: {}",
            presentation::language_name(&self.current.entry.language)
        ));
        Ok(())
    }

    fn cycle_category(&mut self) -> Result<()> {
        let current = self.category.as_deref().unwrap_or(DEFAULT_CATEGORY);
        let start = CATEGORY_CYCLE
            .iter()
            .position(|category| *category == current)
            .unwrap_or(0);
        let language = self.current.entry.language.clone();

        for offset in 1..=CATEGORY_CYCLE.len() {
            let next = CATEGORY_CYCLE[(start + offset) % CATEGORY_CYCLE.len()];
            let candidate = self
                .store
                .random(&language, Some(next), 1)?
                .into_iter()
                .next();
            if let Some(saying) = candidate {
                self.category = Some(next.to_owned());
                self.current = saying;
                self.reload_variants()?;
                self.message = Some(format!("Categoría: {}", presentation::category_name(next)));
                return Ok(());
            }
        }
        self.message = Some("Sin entradas en otras categorías para este idioma".to_owned());
        Ok(())
    }

    fn open_source(&mut self) {
        self.message = Some(match webbrowser::open(&self.current.entry.source_url) {
            Ok(()) => "Fuente abierta en el navegador".to_owned(),
            Err(error) => format!("No se pudo abrir la fuente: {error}"),
        });
    }
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or_default()
}

/// Spanish leads the list: it is the reference language of the catalog.
fn spanish_first(codes: &mut Vec<String>) {
    if let Some(index) = codes.iter().position(|code| code == "es") {
        if index != 0 {
            let spanish = codes.remove(index);
            codes.insert(0, spanish);
        }
    }
}

struct TerminalGuard {
    active: bool,
}

impl TerminalGuard {
    fn enter() -> Result<Self> {
        terminal::enable_raw_mode().context("enabling raw mode")?;
        // Own the guard before the alternate screen so a failure still restores raw mode.
        let guard = Self { active: true };
        execute!(io::stdout(), EnterAlternateScreen, Hide)
            .context("entering the alternate screen")?;
        Ok(guard)
    }

    fn leave(&mut self) -> Result<()> {
        if !self.active {
            return Ok(());
        }
        self.active = false;
        let screen = execute!(io::stdout(), Show, LeaveAlternateScreen)
            .context("leaving the alternate screen");
        // Raw mode must be restored even when the screen restoration fails.
        let raw = terminal::disable_raw_mode().context("disabling raw mode");
        match (screen, raw) {
            (Err(error), _) => Err(error),
            (Ok(()), Err(error)) => Err(error),
            (Ok(()), Ok(())) => Ok(()),
        }
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = self.leave();
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::Session;
    use crate::presentation::ColorMode;
    use crate::store::{RefreshProgress, Store};

    fn store() -> Store {
        Store::open(Path::new(":memory:")).unwrap()
    }

    #[test]
    fn starts_with_a_matching_entry() {
        let store = store();
        let session = Session::start(&store, "es", None, ColorMode::Never).unwrap();
        assert_eq!(session.current.entry.language, "es");
        assert_eq!(session.category.as_deref(), Some("refran"));
    }

    #[test]
    fn cycles_multilingual_variants_of_the_same_group() {
        let store = store();
        let catalan = store
            .by_group("cvc:59530")
            .unwrap()
            .into_iter()
            .find(|saying| saying.entry.language == "ca")
            .unwrap();
        let mut session =
            Session::with_current(&store, catalan, Some("refran".to_owned()), ColorMode::Never)
                .unwrap();
        assert_eq!(session.variants.len(), 2);
        assert_eq!(session.next_language().as_deref(), Some("en"));

        session.cycle_language(1).unwrap();
        assert_eq!(session.current.entry.language, "en");
        assert_eq!(session.current.entry.group_id.as_deref(), Some("cvc:59530"));

        session.cycle_language(-1).unwrap();
        assert_eq!(session.current.entry.language, "ca");
    }

    #[test]
    fn strip_lists_the_variants_of_the_current_group() {
        let store = store();
        let catalan = store
            .by_group("cvc:59530")
            .unwrap()
            .into_iter()
            .find(|saying| saying.entry.language == "ca")
            .unwrap();
        let session =
            Session::with_current(&store, catalan, Some("refran".to_owned()), ColorMode::Never)
                .unwrap();
        let (label, codes) = session.language_strip();
        assert_eq!(label.as_deref(), Some("Otros idiomas"));
        assert_eq!(codes, ["ca", "en"]);
    }

    #[test]
    fn strip_keeps_the_language_of_a_single_language_entry() {
        let store = store();
        let spanish = store
            .random("es", Some("refran"), 20)
            .unwrap()
            .into_iter()
            .find(|saying| saying.entry.group_id.is_none())
            .unwrap();
        let session =
            Session::with_current(&store, spanish, Some("refran".to_owned()), ColorMode::Never)
                .unwrap();
        let (label, codes) = session.language_strip();
        assert_eq!(label.as_deref(), Some("Idioma"));
        assert_eq!(codes, ["es"]);
    }

    #[test]
    fn cycle_language_never_swaps_the_entry() {
        let store = store();
        let spanish = store
            .random("es", Some("refran"), 20)
            .unwrap()
            .into_iter()
            .find(|saying| saying.entry.group_id.is_none())
            .unwrap();
        let original = spanish.id;
        let mut session =
            Session::with_current(&store, spanish, Some("refran".to_owned()), ColorMode::Never)
                .unwrap();

        session.cycle_language(1).unwrap();
        assert_eq!(session.current.id, original);
        assert_eq!(session.current.entry.language, "es");
        let (_, codes) = session.language_strip();
        assert_eq!(codes, ["es"], "the strip must keep Spanish visible");
        assert!(
            session
                .message
                .as_deref()
                .is_some_and(|message| message.contains("solo está en Español")),
            "{:?}",
            session.message
        );
    }

    #[test]
    fn cycles_categories_that_have_entries() {
        let store = store();
        let mut session = Session::start(&store, "es", None, ColorMode::Never).unwrap();
        session.cycle_category().unwrap();
        assert_eq!(session.category.as_deref(), Some("proverbio"));
        assert_eq!(session.current.entry.category, "proverbio");
    }

    #[test]
    fn cycle_category_skips_categories_without_entries() {
        let store = store();
        let dicho = store
            .random("es", Some("dicho"), 1)
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        let mut session =
            Session::with_current(&store, dicho, Some("dicho".to_owned()), ColorMode::Never)
                .unwrap();

        session.cycle_category().unwrap();
        assert_ne!(session.category.as_deref(), Some("dicho"));
        assert_eq!(
            session.category.as_deref(),
            Some(session.current.entry.category.as_str())
        );
        assert!(session
            .message
            .as_deref()
            .is_some_and(|message| message.starts_with("Categoría:")));
    }

    #[test]
    fn shows_and_clears_worker_progress() {
        let path = std::env::temp_dir().join(format!(
            "refranero-progress-{}-{:?}.sqlite3",
            std::process::id(),
            std::thread::current().id()
        ));
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
        }

        let reader = Store::open(&path).unwrap();
        let mut writer = Store::open(&path).unwrap();
        let saying = reader
            .random("es", Some("refran"), 1)
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        let mut session =
            Session::with_current(&reader, saying, Some("refran".to_owned()), ColorMode::Never)
                .unwrap();
        assert!(session.progress.is_none());

        writer
            .set_refresh_progress(&RefreshProgress {
                phase: "Wayback A".into(),
                done: 3,
                total: 10,
                updated_at: super::unix_now(),
            })
            .unwrap();
        session.poll_refresh();
        let view = session.progress.as_ref().expect("progress view");
        assert_eq!(view.label, "Actualizando Wayback A");
        assert_eq!((view.done, view.total), (3, 10));

        // A stale bar from a dead worker must not stick.
        writer
            .set_refresh_progress(&RefreshProgress {
                phase: "Wayback A".into(),
                done: 9,
                total: 10,
                updated_at: 0,
            })
            .unwrap();
        session.poll_refresh();
        assert!(session.progress.is_none());

        writer
            .try_acquire_refresh_lease(super::unix_now(), "test-worker")
            .unwrap();
        writer.clear_refresh_progress("test-worker").unwrap();
        session.poll_refresh();
        assert!(session.progress.is_none());

        drop(session);
        drop(writer);
        drop(reader);
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
        }
    }

    #[test]
    fn notices_when_the_catalog_was_refreshed_in_parallel() {
        let path = std::env::temp_dir().join(format!(
            "refranero-notice-{}-{:?}.sqlite3",
            std::process::id(),
            std::thread::current().id()
        ));
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
        }

        let reader = Store::open(&path).unwrap();
        let mut writer = Store::open(&path).unwrap();
        let saying = reader
            .random("es", Some("refran"), 1)
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        let mut session =
            Session::with_current(&reader, saying, Some("refran".to_owned()), ColorMode::Never)
                .unwrap();

        session.poll_refresh();
        assert!(session.message.is_none(), "{:?}", session.message);

        writer.record_successful_refresh(1_700_000_000).unwrap();
        session.poll_refresh();
        assert_eq!(session.message.as_deref(), Some("Catálogo actualizado"));
        assert!(session.dirty);

        // The same generation must not repeat the notice.
        session.message = None;
        session.poll_refresh();
        assert!(session.message.is_none(), "{:?}", session.message);

        // A Wayback-only refresh must also be announced.
        writer.record_wayback_success('A', 1_700_000_500).unwrap();
        session.poll_refresh();
        assert_eq!(session.message.as_deref(), Some("Catálogo actualizado"));

        drop(session);
        drop(writer);
        drop(reader);
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
        }
    }
}
