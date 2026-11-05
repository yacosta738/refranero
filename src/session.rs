//! Interactive reading session: terminal lifecycle, key handling and state transitions.
//!
//! The session owns I/O and catalog access. Composition stays in [`crate::presentation`], so the
//! layout keeps its snapshot coverage.
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use crossterm::cursor::{Hide, MoveTo, Show};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::style::Print;
use crossterm::terminal::{self, Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::{execute, queue};

use crate::domain::{Category, Saying};
use crate::presentation::{
    self, compose, Catalog, ColorMode, Message, ProgressView, Screen, UiLanguage,
};
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

/// Launcher of the forced refresh worker.
///
/// The session depends on this narrow function instead of the process-spawning code in `main`,
/// so the update request stays testable without starting a real worker.
pub type UpdateSpawner = Box<dyn Fn(&Path, &str) -> Result<()>>;

struct UpdateConfig {
    db: PathBuf,
    offline: bool,
    spawn: UpdateSpawner,
}

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
    observed_refresh_error: Option<i64>,
    pending_update_notice: bool,
    progress: Option<ProgressView>,
    dirty: bool,
    color: bool,
    links: bool,
    catalog: Catalog,
    updates: Option<UpdateConfig>,
}

impl<'a> Session<'a> {
    /// Load a first random entry and its multilingual variants.
    pub fn start(
        store: &'a Store,
        language: &str,
        category: Option<Category>,
        color_mode: ColorMode,
        ui_language: UiLanguage,
    ) -> Result<Self> {
        let category = category
            .map(|value| value.as_str().to_owned())
            .unwrap_or_else(|| DEFAULT_CATEGORY.to_owned());
        let current = store
            .random(language, Some(&category), 1)?
            .into_iter()
            .next()
            .with_context(|| format!("no local entries for lang={language} category={category}"))?;
        Self::with_current(store, current, Some(category), color_mode, ui_language)
    }

    fn with_current(
        store: &'a Store,
        current: Saying,
        category: Option<String>,
        color_mode: ColorMode,
        ui_language: UiLanguage,
    ) -> Result<Self> {
        let options = presentation::detect(color_mode);
        let observed_refresh = store.last_successful_refresh()?;
        let observed_wayback = store.last_wayback_refresh()?;
        let observed_refresh_error = store.refresh_error()?.map(|error| error.updated_at);
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
            observed_refresh_error,
            pending_update_notice: false,
            progress: None,
            dirty: true,
            color: options.color,
            links: options.links,
            catalog: Catalog::new(ui_language),
            updates: None,
        };
        session.reload_variants()?;
        Ok(session)
    }

    /// Enable the interactive update action.
    pub fn with_updates(mut self, db: PathBuf, offline: bool, spawn: UpdateSpawner) -> Self {
        self.updates = Some(UpdateConfig { db, offline, spawn });
        self
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
            KeyCode::Char('u') => self.request_update(),
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
            language: self.catalog.language(),
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
        self.poll_generation();
        self.poll_refresh_error();
        self.poll_progress();
        if self.progress.is_none() && self.pending_update_notice {
            self.pending_update_notice = false;
            self.message = Some(self.catalog.text(Message::CatalogUpdated).to_owned());
            self.dirty = true;
        }
    }

    fn poll_refresh_error(&mut self) {
        let Ok(error) = self.store.refresh_error() else {
            return;
        };
        let Some(error) = error else {
            self.observed_refresh_error = None;
            return;
        };
        if Some(error.updated_at) == self.observed_refresh_error {
            return;
        }
        self.observed_refresh_error = Some(error.updated_at);
        // Keep the status line readable on narrow terminals; the log keeps the full error.
        let detail: String = error.message.chars().take(120).collect();
        self.message = Some(self.catalog.format(Message::UpdateFailedRun, &detail));
        self.dirty = true;
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
                label: format!(
                    "{} {}",
                    self.catalog.text(Message::ProgressUpdating),
                    progress.phase
                ),
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
        if self.progress.is_some() {
            // Wait for the full update run (including subsequent phases) before announcing completion.
            self.pending_update_notice = true;
        } else {
            self.message = Some(self.catalog.text(Message::CatalogUpdated).to_owned());
            self.dirty = true;
        }
    }

    fn language_strip(&self) -> (Option<String>, Vec<String>) {
        if self.variants.len() > 1 {
            let mut codes: Vec<String> = self
                .variants
                .iter()
                .map(|saying| saying.entry.language.clone())
                .collect();
            spanish_first(&mut codes);
            (
                Some(self.catalog.text(Message::StripOtherLanguages).to_owned()),
                codes,
            )
        } else {
            // A single-language entry still shows its language: nothing to hide.
            (
                Some(self.catalog.text(Message::StripLanguage).to_owned()),
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
            self.message = Some(self.catalog.text(Message::NoMoreEntries).to_owned());
        }
        Ok(())
    }

    fn cycle_language(&mut self, direction: isize) -> Result<()> {
        if self.variants.len() < 2 {
            let language = self.catalog.language_name(&self.current.entry.language);
            self.message = Some(self.catalog.format(Message::OnlyInLanguage, &language));
            return Ok(());
        }
        let len = self.variants.len() as isize;
        self.variant_index = (self.variant_index as isize + direction).rem_euclid(len) as usize;
        self.current = self.variants[self.variant_index].clone();
        let language = self.catalog.language_name(&self.current.entry.language);
        self.message = Some(self.catalog.format(Message::CurrentLanguage, &language));
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
                let category = self.catalog.category(next);
                self.message = Some(self.catalog.format(Message::CurrentCategory, &category));
                return Ok(());
            }
        }
        self.message = Some(self.catalog.text(Message::NoOtherCategories).to_owned());
        Ok(())
    }

    fn open_source(&mut self) {
        self.message = Some(match webbrowser::open(&self.current.entry.source_url) {
            Ok(()) => self.catalog.text(Message::SourceOpened).to_owned(),
            Err(error) => self
                .catalog
                .format(Message::SourceOpenFailed, &error.to_string()),
        });
    }

    /// Start a forced refresh without blocking the event loop.
    ///
    /// The session owns the lease before spawning the worker, so a second `u` cannot start a
    /// competing writer. The worker publishes progress for [`Session::poll_progress`].
    fn request_update(&mut self) {
        let Some(updates) = self.updates.as_ref() else {
            self.message = Some(self.catalog.text(Message::UpdateUnavailable).to_owned());
            self.dirty = true;
            return;
        };
        if updates.offline {
            self.message = Some(self.catalog.text(Message::UpdateOffline).to_owned());
            self.dirty = true;
            return;
        }

        let db = updates.db.clone();
        let owner = update_owner();
        match self.store.try_acquire_refresh_lease(unix_now(), &owner) {
            Ok(true) => match (updates.spawn)(&db, &owner) {
                Ok(()) => {
                    self.message = Some(self.catalog.text(Message::UpdateStarted).to_owned());
                }
                Err(error) => {
                    let _ = self.store.release_refresh_lease(&owner);
                    self.message = Some(
                        self.catalog
                            .format(Message::UpdateFailed, &error.to_string()),
                    );
                }
            },
            Ok(false) => {
                self.message = Some(self.catalog.text(Message::UpdateAlreadyRunning).to_owned());
            }
            Err(error) => {
                self.message = Some(
                    self.catalog
                        .format(Message::UpdateFailed, &error.to_string()),
                );
            }
        }
        self.dirty = true;
    }
}

fn update_owner() -> String {
    format!("{}-{}", std::process::id(), unix_now())
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
    use std::cell::RefCell;
    use std::path::{Path, PathBuf};
    use std::rc::Rc;

    use crossterm::event::{KeyCode, KeyModifiers};

    use super::{Session, UpdateSpawner};
    use crate::presentation::{ColorMode, Message, UiLanguage};
    use crate::store::{RefreshProgress, Store};

    fn store() -> Store {
        Store::open(Path::new(":memory:")).unwrap()
    }

    fn update_session<'a>(store: &'a Store, offline: bool, spawn: UpdateSpawner) -> Session<'a> {
        let saying = store
            .random("es", Some("refran"), 1)
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        Session::with_current(
            store,
            saying,
            Some("refran".to_owned()),
            ColorMode::Never,
            UiLanguage::Spanish,
        )
        .unwrap()
        .with_updates(PathBuf::from("/tmp/refranero-test.sqlite3"), offline, spawn)
    }

    #[test]
    fn starts_with_a_matching_entry() {
        let store = store();
        let session =
            Session::start(&store, "es", None, ColorMode::Never, UiLanguage::Spanish).unwrap();
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
        let mut session = Session::with_current(
            &store,
            catalan,
            Some("refran".to_owned()),
            ColorMode::Never,
            UiLanguage::Spanish,
        )
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
        let session = Session::with_current(
            &store,
            catalan,
            Some("refran".to_owned()),
            ColorMode::Never,
            UiLanguage::Spanish,
        )
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
        let session = Session::with_current(
            &store,
            spanish,
            Some("refran".to_owned()),
            ColorMode::Never,
            UiLanguage::Spanish,
        )
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
        let mut session = Session::with_current(
            &store,
            spanish,
            Some("refran".to_owned()),
            ColorMode::Never,
            UiLanguage::Spanish,
        )
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
    fn english_session_messages_and_strip_labels_are_localized() {
        let store = store();
        let saying = store
            .random("es", Some("refran"), 20)
            .unwrap()
            .into_iter()
            .find(|saying| saying.entry.group_id.is_none())
            .unwrap();
        let mut session = Session::with_current(
            &store,
            saying,
            Some("refran".to_owned()),
            ColorMode::Never,
            UiLanguage::English,
        )
        .unwrap();

        assert_eq!(session.language_strip().0.as_deref(), Some("Language"));
        session.cycle_language(1).unwrap();
        assert!(session
            .message
            .as_deref()
            .is_some_and(|message| message.contains("only available in Spanish")));
        session.poll_generation();
        assert_eq!(
            session.catalog.text(Message::CatalogUpdated),
            "Catalog updated"
        );
    }

    #[test]
    fn cycles_categories_that_have_entries() {
        let store = store();
        let mut session =
            Session::start(&store, "es", None, ColorMode::Never, UiLanguage::Spanish).unwrap();
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
        let mut session = Session::with_current(
            &store,
            dicho,
            Some("dicho".to_owned()),
            ColorMode::Never,
            UiLanguage::Spanish,
        )
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
    fn manual_update_starts_a_worker_and_holds_the_lease() {
        let store = store();
        let calls = Rc::new(RefCell::new(Vec::new()));
        let recorded = Rc::clone(&calls);
        let spawn: UpdateSpawner = Box::new(move |path, owner| {
            recorded
                .borrow_mut()
                .push((path.to_path_buf(), owner.to_owned()));
            Ok(())
        });
        let mut session = update_session(&store, false, spawn);

        assert!(session
            .handle(KeyCode::Char('u'), KeyModifiers::NONE)
            .unwrap());

        let calls = calls.borrow();
        assert_eq!(calls.len(), 1, "one key press must start one worker");
        assert_eq!(calls[0].0, PathBuf::from("/tmp/refranero-test.sqlite3"));
        assert_eq!(
            session.message.as_deref(),
            Some("Actualización iniciada en segundo plano")
        );
        assert!(
            !store
                .try_acquire_refresh_lease(super::unix_now() + 1, "other")
                .unwrap(),
            "the spawned owner must keep the lease"
        );
    }

    #[test]
    fn manual_update_is_disabled_in_offline_mode() {
        let store = store();
        let called = Rc::new(RefCell::new(false));
        let spawn_called = Rc::clone(&called);
        let spawn: UpdateSpawner = Box::new(move |_, _| {
            *spawn_called.borrow_mut() = true;
            Ok(())
        });
        let mut session = update_session(&store, true, spawn);

        session
            .handle(KeyCode::Char('u'), KeyModifiers::NONE)
            .unwrap();

        assert!(!*called.borrow(), "offline mode must not start a worker");
        assert_eq!(
            session.message.as_deref(),
            Some("Modo sin conexión: actualización desactivada")
        );
    }

    #[test]
    fn manual_update_does_not_duplicate_a_running_worker() {
        let store = store();
        assert!(store
            .try_acquire_refresh_lease(super::unix_now(), "other-worker")
            .unwrap());
        let called = Rc::new(RefCell::new(false));
        let spawn_called = Rc::clone(&called);
        let spawn: UpdateSpawner = Box::new(move |_, _| {
            *spawn_called.borrow_mut() = true;
            Ok(())
        });
        let mut session = update_session(&store, false, spawn);

        session
            .handle(KeyCode::Char('u'), KeyModifiers::NONE)
            .unwrap();

        assert!(!*called.borrow());
        assert_eq!(
            session.message.as_deref(),
            Some("Ya hay una actualización en curso")
        );
    }

    #[test]
    fn manual_update_releases_the_lease_when_the_worker_cannot_start() {
        let store = store();
        let spawn: UpdateSpawner = Box::new(|_, _| Err(anyhow::anyhow!("worker unavailable")));
        let mut session = update_session(&store, false, spawn);

        session
            .handle(KeyCode::Char('u'), KeyModifiers::NONE)
            .unwrap();

        assert!(
            store
                .try_acquire_refresh_lease(super::unix_now() + 1, "other")
                .unwrap(),
            "a failed spawn must not keep the refresh lease"
        );
        assert!(session
            .message
            .as_deref()
            .is_some_and(|message| message.contains("worker unavailable")));
    }

    #[test]
    fn shows_and_clears_worker_progress() {
        let path = std::env::temp_dir().join(format!(
            "refranero-progress-{}-{:?}.sqlite3",
            // Keep the temp path unique per test thread.
            std::process::id(),
            std::thread::current().id()
        ));
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
        }

        let reader = Store::open(&path).unwrap();
        let writer = Store::open(&path).unwrap();
        let saying = reader
            .random("es", Some("refran"), 1)
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        let mut session = Session::with_current(
            &reader,
            saying,
            Some("refran".to_owned()),
            ColorMode::Never,
            UiLanguage::Spanish,
        )
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
        let mut session = Session::with_current(
            &reader,
            saying,
            Some("refran".to_owned()),
            ColorMode::Never,
            UiLanguage::Spanish,
        )
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
