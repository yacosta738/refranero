//! Interface language, message catalog and locale resolution.
//!
//! The interface language is independent from the language of the content: `--lang` filters the
//! sayings and `--ui-lang` selects the language of the labels. Every string the interface shows
//! comes from [`Catalog`], so the renderer and the session never read the environment.

use std::ffi::OsString;

use super::label;

/// Language of the terminal interface.
#[derive(Debug, Clone, Copy, Default, Eq, PartialEq, Hash, clap::ValueEnum)]
pub enum UiLanguage {
    /// Spanish, the reference language of the catalog.
    #[default]
    #[value(name = "es")]
    Spanish,
    /// English.
    #[value(name = "en")]
    English,
}

/// A static interface string.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum Message {
    /// Title of the help screen.
    HelpTitle,
    /// Key column of the long help, next entry.
    HelpNextKeys,
    /// Key column of the long help, next language.
    HelpNextLanguageKeys,
    /// Key column of the long help, previous language.
    HelpPreviousLanguageKeys,
    /// Key column of the long help, quit.
    HelpQuitKeys,
    /// Compact help, next entry.
    HelpNextShort,
    /// Compact help, language.
    HelpLanguageShort,
    /// Compact help, previous entry.
    HelpPreviousShort,
    /// Compact help, category.
    HelpCategoryShort,
    /// Compact help, open the source.
    HelpOpenShort,
    /// Compact help, update the catalog.
    HelpUpdateShort,
    /// Compact help, close the help.
    HelpCloseShort,
    /// Compact help, quit.
    HelpQuitShort,
    /// Long help, next entry.
    HelpNextLong,
    /// Long help, next language or variant.
    HelpNextLanguageLong,
    /// Long help, previous language or variant.
    HelpPreviousLanguageLong,
    /// Long help, next category.
    HelpCategoryLong,
    /// Long help, open the source.
    HelpOpenLong,
    /// Long help, update the catalog.
    HelpUpdateLong,
    /// Long help, close the help.
    HelpCloseLong,
    /// Long help, quit.
    HelpQuitLong,
    /// Footer, next entry.
    FooterNext,
    /// Footer, open the source.
    FooterOpen,
    /// Footer, language.
    FooterLanguage,
    /// Footer, category.
    FooterCategory,
    /// Footer, help.
    FooterHelp,
    /// Footer, update the catalog.
    FooterUpdate,
    /// Footer, quit.
    FooterQuit,
    /// Label of the source hyperlink.
    LinkLabel,
    /// Short caption of the language strip.
    LanguageStripShort,
    /// Brand shown in collection headers.
    CollectionBrand,
    /// Plural fallback for mixed collections.
    EntriesFallback,
    /// Caption of the language strip when several languages are available.
    StripOtherLanguages,
    /// Caption of the language strip when a single language is available.
    StripLanguage,
    /// Notice shown when the filter has no more entries.
    NoMoreEntries,
    /// Notice shown when no other category has entries.
    NoOtherCategories,
    /// Prefix used by the background refresh progress line.
    ProgressUpdating,
    /// Notice shown when the background refresh updated the catalog.
    CatalogUpdated,
    /// Notice shown when a manual update starts.
    UpdateStarted,
    /// Notice shown when another worker already owns the update.
    UpdateAlreadyRunning,
    /// Notice shown when the reader runs in offline mode.
    UpdateOffline,
    /// Notice shown when the reader has no update support configured.
    UpdateUnavailable,
    /// Notice shown when the update worker cannot start.
    UpdateFailed,
    /// Notice shown when the update worker finishes with an error.
    UpdateFailedRun,
    /// Notice shown when the source opens in the browser.
    SourceOpened,
    /// Notice shown when the entry exists in a single language.
    OnlyInLanguage,
    /// Notice shown when the reader changes the language.
    CurrentLanguage,
    /// Notice shown when the reader changes the category.
    CurrentCategory,
    /// Notice shown when the browser cannot open the source.
    SourceOpenFailed,
    /// Title of the statistics screen.
    StatsTitle,
    /// Prefix of the total line of the statistics screen.
    StatsTotal,
    /// Unit of the total line of the statistics screen.
    StatsEntries,
    /// Heading of the category breakdown.
    StatsByCategory,
    /// Heading of the language breakdown.
    StatsByLanguage,
    /// Empty statistics screen.
    StatsEmpty,
    /// Compact help, statistics.
    HelpStatsShort,
    /// Long help, statistics.
    HelpStatsLong,
    /// Footer, statistics.
    FooterStats,
    /// Footer of the statistics screen, back to the entry.
    StatsBack,
}

/// Every message of the catalog, so tests can prove that no variant is missing.
#[cfg(test)]
const ALL_MESSAGES: &[Message] = &[
    Message::HelpTitle,
    Message::HelpNextKeys,
    Message::HelpNextLanguageKeys,
    Message::HelpPreviousLanguageKeys,
    Message::HelpQuitKeys,
    Message::HelpNextShort,
    Message::HelpLanguageShort,
    Message::HelpPreviousShort,
    Message::HelpCategoryShort,
    Message::HelpOpenShort,
    Message::HelpUpdateShort,
    Message::HelpCloseShort,
    Message::HelpQuitShort,
    Message::HelpNextLong,
    Message::HelpNextLanguageLong,
    Message::HelpPreviousLanguageLong,
    Message::HelpCategoryLong,
    Message::HelpOpenLong,
    Message::HelpUpdateLong,
    Message::HelpCloseLong,
    Message::HelpQuitLong,
    Message::FooterNext,
    Message::FooterOpen,
    Message::FooterLanguage,
    Message::FooterCategory,
    Message::FooterHelp,
    Message::FooterUpdate,
    Message::FooterQuit,
    Message::LinkLabel,
    Message::LanguageStripShort,
    Message::CollectionBrand,
    Message::EntriesFallback,
    Message::StripOtherLanguages,
    Message::StripLanguage,
    Message::NoMoreEntries,
    Message::NoOtherCategories,
    Message::ProgressUpdating,
    Message::CatalogUpdated,
    Message::UpdateStarted,
    Message::UpdateAlreadyRunning,
    Message::UpdateOffline,
    Message::UpdateUnavailable,
    Message::UpdateFailed,
    Message::UpdateFailedRun,
    Message::SourceOpened,
    Message::OnlyInLanguage,
    Message::CurrentLanguage,
    Message::CurrentCategory,
    Message::SourceOpenFailed,
];

/// The message catalog of one interface language.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct Catalog {
    language: UiLanguage,
}

impl Catalog {
    /// Catalog of the given interface language.
    pub const fn new(language: UiLanguage) -> Self {
        Self { language }
    }

    /// Interface language of this catalog.
    pub const fn language(&self) -> UiLanguage {
        self.language
    }

    /// Static text of a message.
    pub const fn text(&self, message: Message) -> &'static str {
        match self.language {
            UiLanguage::Spanish => spanish(message),
            UiLanguage::English => english(message),
        }
    }

    /// Text of a message that carries one value.
    pub fn format(&self, message: Message, value: &str) -> String {
        let prefix = match (self.language, message) {
            (UiLanguage::Spanish, Message::OnlyInLanguage) => "Este refrán solo está en ",
            (UiLanguage::English, Message::OnlyInLanguage) => "This saying is only available in ",
            (UiLanguage::Spanish, Message::CurrentLanguage) => "Idioma: ",
            (UiLanguage::English, Message::CurrentLanguage) => "Language: ",
            (UiLanguage::Spanish, Message::CurrentCategory) => "Categoría: ",
            (UiLanguage::English, Message::CurrentCategory) => "Category: ",
            (UiLanguage::Spanish, Message::SourceOpenFailed) => "No se pudo abrir la fuente: ",
            (UiLanguage::English, Message::SourceOpenFailed) => "Could not open the source: ",
            (UiLanguage::Spanish, Message::UpdateFailed) => "No se pudo iniciar la actualización: ",
            (UiLanguage::English, Message::UpdateFailed) => "Could not start the update: ",
            (UiLanguage::Spanish, Message::UpdateFailedRun) => "La actualización falló: ",
            (UiLanguage::English, Message::UpdateFailedRun) => "The update failed: ",
            _ => return self.text(message).to_owned(),
        };
        format!("{prefix}{value}")
    }

    /// Readable category name.
    pub fn category(&self, value: &str) -> String {
        label::category(self.language, value)
    }

    /// Plural label used by collection headers.
    pub fn category_plural(&self, value: &str) -> &'static str {
        label::plural(self.language, value)
    }

    /// Readable source name.
    pub fn source_name(&self, value: &str) -> String {
        label::source(self.language, value)
    }

    /// Readable language name.
    pub fn language_name(&self, code: &str) -> String {
        label::language(self.language, code)
    }

    /// Fase publicada por el worker, en el idioma de la interfaz lectora.
    pub fn progress_phase(&self, phase: &str) -> String {
        if !phase.starts_with("Wayback ") || self.language == UiLanguage::English {
            return phase.to_owned();
        }
        phase
            .split_whitespace()
            .map(|word| match word {
                "index" => "índice",
                "root" => "ficha",
                "request" => "solicitud",
                "waiting/retry" => "espera/reintento",
                _ => word,
            })
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// Spanish text of every message.
const fn spanish(message: Message) -> &'static str {
    match message {
        Message::HelpTitle => "ATAJOS",
        Message::HelpNextKeys => "n / Enter / Espacio",
        Message::HelpNextLanguageKeys => "l / →",
        Message::HelpPreviousLanguageKeys => "h / ←",
        Message::HelpQuitKeys => "q / Esc",
        Message::HelpNextShort => "otro",
        Message::HelpLanguageShort => "idioma",
        Message::HelpPreviousShort => "anterior",
        Message::HelpCategoryShort => "categoría",
        Message::HelpOpenShort => "abrir",
        Message::HelpUpdateShort => "actualizar",
        Message::HelpStatsShort => "estadísticas",
        Message::HelpCloseShort => "cerrar",
        Message::HelpQuitShort => "salir",
        Message::HelpNextLong => "Otro refrán",
        Message::HelpNextLanguageLong => "Siguiente idioma o variante",
        Message::HelpPreviousLanguageLong => "Idioma o variante anterior",
        Message::HelpCategoryLong => "Siguiente categoría",
        Message::HelpOpenLong => "Abrir la fuente",
        Message::HelpUpdateLong => "Actualizar el catálogo ahora",
        Message::HelpStatsLong => "Ver las estadísticas del catálogo",
        Message::HelpCloseLong => "Cerrar esta ayuda",
        Message::HelpQuitLong => "Salir",
        Message::FooterNext => "n otro",
        Message::FooterOpen => "o abrir",
        Message::FooterLanguage => "l/h idioma",
        Message::FooterCategory => "c categoría",
        Message::FooterHelp => "? ayuda",
        Message::FooterUpdate => "u actualizar",
        Message::FooterStats => "s stats",
        Message::FooterQuit => "q salir",
        Message::StatsTitle => "ESTADÍSTICAS",
        Message::StatsTotal => "Total",
        Message::StatsEntries => "entradas",
        Message::StatsByCategory => "POR CATEGORÍA",
        Message::StatsByLanguage => "POR IDIOMA",
        Message::StatsEmpty => "El catálogo está vacío",
        Message::StatsBack => "s volver",
        Message::LinkLabel => "Consultar fuente",
        Message::LanguageStripShort => "Idiomas",
        Message::CollectionBrand => "REFRANERO",
        Message::EntriesFallback => "ENTRADAS",
        Message::StripOtherLanguages => "Otros idiomas",
        Message::StripLanguage => "Idioma",
        Message::NoMoreEntries => "No hay más refranes con este filtro",
        Message::NoOtherCategories => "Sin entradas en otras categorías para este idioma",
        Message::ProgressUpdating => "Actualizando",
        Message::CatalogUpdated => "Catálogo actualizado",
        Message::UpdateStarted => "Actualización iniciada en segundo plano",
        Message::UpdateAlreadyRunning => "Ya hay una actualización en curso",
        Message::UpdateOffline => "Modo sin conexión: actualización desactivada",
        Message::UpdateUnavailable => "Actualización no disponible",
        Message::UpdateFailed => "No se pudo iniciar la actualización: {value}",
        Message::UpdateFailedRun => "La actualización falló: {value}",
        Message::SourceOpened => "Fuente abierta en el navegador",
        Message::OnlyInLanguage => "Este refrán solo está en {value}",
        Message::CurrentLanguage => "Idioma: {value}",
        Message::CurrentCategory => "Categoría: {value}",
        Message::SourceOpenFailed => "No se pudo abrir la fuente: {value}",
    }
}

/// English text of every message.
const fn english(message: Message) -> &'static str {
    match message {
        Message::HelpTitle => "SHORTCUTS",
        Message::HelpNextKeys => "n / Enter / Space",
        Message::HelpNextLanguageKeys => "l / →",
        Message::HelpPreviousLanguageKeys => "h / ←",
        Message::HelpQuitKeys => "q / Esc",
        Message::HelpNextShort => "next",
        Message::HelpLanguageShort => "language",
        Message::HelpPreviousShort => "previous",
        Message::HelpCategoryShort => "category",
        Message::HelpOpenShort => "open",
        Message::HelpUpdateShort => "update",
        Message::HelpStatsShort => "stats",
        Message::HelpCloseShort => "close",
        Message::HelpQuitShort => "quit",
        Message::HelpNextLong => "Another saying",
        Message::HelpNextLanguageLong => "Next language or variant",
        Message::HelpPreviousLanguageLong => "Previous language or variant",
        Message::HelpCategoryLong => "Next category",
        Message::HelpOpenLong => "Open the source",
        Message::HelpUpdateLong => "Update the catalog now",
        Message::HelpStatsLong => "See the catalog statistics",
        Message::HelpCloseLong => "Close this help",
        Message::HelpQuitLong => "Quit",
        Message::FooterNext => "n next",
        Message::FooterOpen => "o open",
        Message::FooterLanguage => "l/h language",
        Message::FooterCategory => "c category",
        Message::FooterHelp => "? help",
        Message::FooterUpdate => "u update",
        Message::FooterStats => "s stats",
        Message::FooterQuit => "q quit",
        Message::StatsTitle => "STATISTICS",
        Message::StatsTotal => "Total",
        Message::StatsEntries => "entries",
        Message::StatsByCategory => "BY CATEGORY",
        Message::StatsByLanguage => "BY LANGUAGE",
        Message::StatsEmpty => "The catalog is empty",
        Message::StatsBack => "s back",
        Message::LinkLabel => "View source",
        Message::LanguageStripShort => "Languages",
        Message::CollectionBrand => "REFRANERO",
        Message::EntriesFallback => "ENTRIES",
        Message::StripOtherLanguages => "Other languages",
        Message::StripLanguage => "Language",
        Message::NoMoreEntries => "No more sayings with this filter",
        Message::NoOtherCategories => "No entries in other categories for this language",
        Message::ProgressUpdating => "Updating",
        Message::CatalogUpdated => "Catalog updated",
        Message::UpdateStarted => "Update started in the background",
        Message::UpdateAlreadyRunning => "An update is already running",
        Message::UpdateOffline => "Offline mode: update disabled",
        Message::UpdateUnavailable => "Update is not available",
        Message::UpdateFailed => "Could not start the update: {value}",
        Message::UpdateFailedRun => "The update failed: {value}",
        Message::SourceOpened => "Source opened in the browser",
        Message::OnlyInLanguage => "This saying is only available in {value}",
        Message::CurrentLanguage => "Language: {value}",
        Message::CurrentCategory => "Category: {value}",
        Message::SourceOpenFailed => "Could not open the source: {value}",
    }
}

/// Environment variables that can carry the interface locale, in priority order.
pub const LOCALE_ENV_VARS: [&str; 3] = ["LC_ALL", "LC_MESSAGES", "LANG"];

/// Parse a POSIX locale tag into an interface language.
///
/// Tags such as `en_US.UTF-8`, `es-ES` and `es_ES@euro` select a language; anything else is
/// unknown and leaves the decision to the caller.
pub fn parse_locale(tag: &str) -> Option<UiLanguage> {
    let tag = tag.trim();
    if tag.is_empty() {
        return None;
    }
    // The character set and the modifier do not change the language.
    let base = tag.split(['.', '@']).next().unwrap_or(tag);
    // `es-ES` and `es_ES` identify the same language.
    let language = base
        .split(['-', '_'])
        .next()
        .unwrap_or(base)
        .to_ascii_lowercase();
    match language.as_str() {
        "es" => Some(UiLanguage::Spanish),
        "en" => Some(UiLanguage::English),
        _ => None,
    }
}

/// Resolve the interface language.
///
/// The explicit flag always wins. Without a flag, the locale comes from `LC_ALL`,
/// `LC_MESSAGES` and `LANG`, in that order. The first variable that carries a locale decides:
/// an unknown locale falls back to Spanish, because the interface only speaks two languages.
pub fn resolve_ui_language<F>(explicit: Option<UiLanguage>, lookup: F) -> UiLanguage
where
    F: Fn(&str) -> Option<OsString>,
{
    if let Some(language) = explicit {
        return language;
    }
    for variable in LOCALE_ENV_VARS {
        let Some(value) = lookup(variable) else {
            continue;
        };
        // A value that is not valid UTF-8 still decides the language: it cannot name a locale,
        // so the interface falls back to Spanish instead of reading the next variable.
        let value = value.to_string_lossy();
        let value = value.trim();
        if value.is_empty() {
            continue;
        }
        return parse_locale(value).unwrap_or_default();
    }
    UiLanguage::default()
}

#[cfg(test)]
mod tests {
    #[test]
    fn wayback_activity_uses_reader_language() {
        let phase = "Wayback bootstrap 1/26 (A) index waiting/retry 7s";
        assert_eq!(
            super::Catalog::new(super::UiLanguage::Spanish).progress_phase(phase),
            "Wayback bootstrap 1/26 (A) índice espera/reintento 7s"
        );
        assert_eq!(
            super::Catalog::new(super::UiLanguage::English).progress_phase(phase),
            phase
        );
    }
    use std::ffi::OsString;

    use super::{parse_locale, resolve_ui_language, Catalog, Message, UiLanguage, ALL_MESSAGES};

    fn env<'a>(values: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<OsString> + 'a {
        move |name| {
            values
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| OsString::from(*value))
        }
    }

    #[test]
    fn locale_tags_select_the_interface_language() {
        assert_eq!(parse_locale("en"), Some(UiLanguage::English));
        assert_eq!(parse_locale("es"), Some(UiLanguage::Spanish));
        assert_eq!(parse_locale("en_US.UTF-8"), Some(UiLanguage::English));
        assert_eq!(parse_locale("es-ES"), Some(UiLanguage::Spanish));
        assert_eq!(parse_locale("es_ES.UTF-8"), Some(UiLanguage::Spanish));
        assert_eq!(parse_locale("es_ES@euro"), Some(UiLanguage::Spanish));
    }

    #[test]
    fn unknown_locales_are_rejected() {
        assert_eq!(parse_locale(""), None);
        assert_eq!(parse_locale("   "), None);
        assert_eq!(parse_locale("fr_FR.UTF-8"), None);
        assert_eq!(parse_locale("C"), None);
        assert_eq!(parse_locale("POSIX"), None);
    }

    #[test]
    fn explicit_language_wins_over_the_environment() {
        let resolved =
            resolve_ui_language(Some(UiLanguage::English), env(&[("LANG", "es_ES.UTF-8")]));
        assert_eq!(resolved, UiLanguage::English);
    }

    #[test]
    fn locale_variables_are_read_in_priority_order() {
        let all = env(&[
            ("LC_ALL", "en_US.UTF-8"),
            ("LC_MESSAGES", "es_ES.UTF-8"),
            ("LANG", "es_ES.UTF-8"),
        ]);
        assert_eq!(resolve_ui_language(None, all), UiLanguage::English);

        // A defined but unsupported high-priority locale is authoritative.
        let unknown_all = env(&[("LC_ALL", "fr_FR.UTF-8"), ("LANG", "en_US.UTF-8")]);
        assert_eq!(resolve_ui_language(None, unknown_all), UiLanguage::Spanish);

        let c_locale = env(&[("LC_ALL", "C"), ("LANG", "en_US.UTF-8")]);
        assert_eq!(resolve_ui_language(None, c_locale), UiLanguage::Spanish);

        let messages = env(&[("LC_MESSAGES", "en_US.UTF-8"), ("LANG", "es_ES.UTF-8")]);
        assert_eq!(resolve_ui_language(None, messages), UiLanguage::English);

        let lang = env(&[("LANG", "en_US.UTF-8")]);
        assert_eq!(resolve_ui_language(None, lang), UiLanguage::English);
    }

    /// Build a locale value that the interface cannot read as a language name.
    #[cfg(unix)]
    fn undecodable_locale() -> OsString {
        use std::os::unix::ffi::OsStringExt;

        // `fr_FR` followed by a byte that is not valid UTF-8.
        OsString::from_vec(vec![b'f', b'r', b'_', b'F', b'R', 0xff])
    }

    /// Build a locale value that the interface cannot read as a language name.
    #[cfg(windows)]
    fn undecodable_locale() -> OsString {
        use std::os::windows::ffi::OsStringExt;

        // `fr_FR` followed by an unpaired surrogate, which is not valid UTF-8 either.
        OsString::from_wide(&[0x0066, 0x0072, 0x005f, 0x0046, 0x0052, 0xd800])
    }

    /// Build a locale value that the interface cannot read as a language name.
    #[cfg(not(any(unix, windows)))]
    fn undecodable_locale() -> OsString {
        // Other platforms have no way to build an undecodable value, so the test
        // relies on a locale that simply does not name a language.
        OsString::from("fr_FR.UTF-8")
    }

    #[test]
    fn an_undecodable_locale_still_decides() {
        // `LC_ALL` holds a value that cannot name a language and `LANG` would select
        // English: the set `LC_ALL` decides, and it cannot name a language, so Spanish
        // is selected.
        let lookup = |name: &str| match name {
            "LC_ALL" => Some(undecodable_locale()),
            "LANG" => Some(OsString::from("en_US.UTF-8")),
            _ => None,
        };
        assert_eq!(resolve_ui_language(None, lookup), UiLanguage::Spanish);
    }

    #[test]
    fn missing_or_unknown_locales_fall_back_to_spanish() {
        assert_eq!(resolve_ui_language(None, env(&[])), UiLanguage::Spanish);
        assert_eq!(
            resolve_ui_language(None, env(&[("LANG", "fr_FR.UTF-8")])),
            UiLanguage::Spanish
        );
        assert_eq!(
            resolve_ui_language(None, env(&[("LANG", "")])),
            UiLanguage::Spanish
        );
    }

    #[test]
    fn every_message_has_text_in_both_languages() {
        for language in [UiLanguage::Spanish, UiLanguage::English] {
            let catalog = Catalog::new(language);
            for message in ALL_MESSAGES {
                assert!(
                    !catalog.text(*message).is_empty(),
                    "{language:?} is missing {message:?}"
                );
            }
        }
    }

    #[test]
    fn messages_carry_their_value() {
        let catalog = Catalog::new(UiLanguage::English);
        assert!(catalog
            .format(Message::CurrentLanguage, "English")
            .contains("English"));
        let spanish = Catalog::new(UiLanguage::Spanish);
        assert!(spanish
            .format(Message::CurrentLanguage, "Español")
            .contains("Español"));
    }

    #[test]
    fn categories_are_translated_into_distinct_english_words() {
        let english = Catalog::new(UiLanguage::English);
        assert_eq!(english.category("refran"), "Saying");
        assert_eq!(english.category("paremia"), "Paremia");
        assert_eq!(english.category("proverbio"), "Proverb");
        assert_eq!(english.category("dicho"), "Adage");
        assert_eq!(english.category("adivinanza"), "Riddle");
        assert_eq!(english.category("cita"), "Literary quote");
        assert_eq!(english.category_plural("refran"), "SAYINGS");
        assert_eq!(english.category_plural("cita"), "LITERARY QUOTES");

        let spanish = Catalog::new(UiLanguage::Spanish);
        assert_eq!(spanish.category("refran"), "Refrán");
        assert_eq!(spanish.category("cita"), "Cita literaria");
        assert_eq!(spanish.category_plural("refran"), "REFRANES");
        assert_eq!(spanish.category_plural("cita"), "CITAS LITERARIAS");
    }

    #[test]
    fn language_names_follow_the_interface_language() {
        let english = Catalog::new(UiLanguage::English);
        assert_eq!(english.language_name("es"), "Spanish");
        assert_eq!(english.language_name("ca"), "Catalan");
        assert_eq!(english.language_name(""), "Unknown");

        let spanish = Catalog::new(UiLanguage::Spanish);
        assert_eq!(spanish.language_name("es"), "Español");
        assert_eq!(spanish.language_name("ca"), "Catalán");
        assert_eq!(spanish.language_name(""), "Desconocido");
    }

    #[test]
    fn source_names_keep_their_own_name_and_translate_the_archive_descriptor() {
        let english = Catalog::new(UiLanguage::English);
        assert_eq!(
            english.source_name("cervantes_virtual"),
            "Cervantes Virtual"
        );
        assert_eq!(
            english.source_name("cvc_wayback"),
            "Centro Virtual Cervantes · Archive"
        );
        assert_eq!(english.source_name(""), "Unknown source");

        let spanish = Catalog::new(UiLanguage::Spanish);
        assert_eq!(
            spanish.source_name("cervantes_virtual"),
            "Cervantes Virtual"
        );
        assert_eq!(
            spanish.source_name("cvc_wayback"),
            "Centro Virtual Cervantes · Archivo"
        );
        assert_eq!(spanish.source_name(""), "Fuente desconocida");
    }
}
