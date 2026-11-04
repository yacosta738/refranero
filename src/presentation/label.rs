//! Human-readable labels for stored identifiers.
//!
//! Labels follow the interface language: the same stored identifier reads differently in Spanish
//! and in English, while the underlying values never change.

use super::i18n::UiLanguage;

/// Readable category name.
pub fn category(ui: UiLanguage, value: &str) -> String {
    match value {
        "refran" => pick(ui, "Refrán", "Saying"),
        "paremia" => pick(ui, "Paremia", "Paremia"),
        "proverbio" => pick(ui, "Proverbio", "Proverb"),
        "dicho" => pick(ui, "Dicho", "Adage"),
        "adivinanza" => pick(ui, "Adivinanza", "Riddle"),
        "cita" => pick(ui, "Cita literaria", "Literary quote"),
        "" => pick(ui, "Desconocido", "Unknown"),
        other => humanize(ui, other),
    }
}

/// Plural label used by collection headers.
pub fn plural(ui: UiLanguage, value: &str) -> &'static str {
    match value {
        "refran" => pick_static(ui, "REFRANES", "SAYINGS"),
        "paremia" => pick_static(ui, "PAREMIAS", "PAREMIAS"),
        "proverbio" => pick_static(ui, "PROVERBIOS", "PROVERBS"),
        "dicho" => pick_static(ui, "DICHOS", "ADAGES"),
        "adivinanza" => pick_static(ui, "ADIVINANZAS", "RIDDLES"),
        "cita" => pick_static(ui, "CITAS LITERARIAS", "LITERARY QUOTES"),
        _ => pick_static(ui, "ENTRADAS", "ENTRIES"),
    }
}

/// Readable language name.
pub fn language(ui: UiLanguage, code: &str) -> String {
    match code {
        "es" => pick(ui, "Español", "Spanish"),
        "ca" => pick(ui, "Catalán", "Catalan"),
        "gl" => pick(ui, "Gallego", "Galician"),
        "eu" => pick(ui, "Euskera", "Basque"),
        "en" => pick(ui, "Inglés", "English"),
        "fr" => pick(ui, "Francés", "French"),
        "de" => pick(ui, "Alemán", "German"),
        "it" => pick(ui, "Italiano", "Italian"),
        "pt" => pick(ui, "Portugués", "Portuguese"),
        "ast" => pick(ui, "Asturiano", "Asturian"),
        "ar" => pick(ui, "Árabe", "Arabic"),
        "he" => pick(ui, "Hebreo", "Hebrew"),
        "zh" => pick(ui, "Chino", "Chinese"),
        "ja" => pick(ui, "Japonés", "Japanese"),
        "" => pick(ui, "Desconocido", "Unknown"),
        other => other.to_uppercase(),
    }
}

/// Readable source name. Unknown values are humanised, never shown as raw identifiers.
pub fn source(ui: UiLanguage, value: &str) -> String {
    match value {
        "cervantes_virtual" => "Cervantes Virtual".to_owned(),
        "cvc" => "Centro Virtual Cervantes".to_owned(),
        "cvc_wayback" => pick(
            ui,
            "Centro Virtual Cervantes · Archivo",
            "Centro Virtual Cervantes · Archive",
        ),
        "" => pick(ui, "Fuente desconocida", "Unknown source"),
        other => humanize(ui, other),
    }
}

/// Pick the Spanish or the English text of the same concept.
fn pick(ui: UiLanguage, spanish: &'static str, english: &'static str) -> String {
    pick_static(ui, spanish, english).to_owned()
}

/// Pick the Spanish or the English text of the same concept.
const fn pick_static(ui: UiLanguage, spanish: &'static str, english: &'static str) -> &'static str {
    match ui {
        UiLanguage::Spanish => spanish,
        UiLanguage::English => english,
    }
}

fn humanize(ui: UiLanguage, value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    for (index, word) in value
        .split(['_', '-'])
        .filter(|word| !word.is_empty())
        .enumerate()
    {
        if index > 0 {
            output.push(' ');
        }
        let mut chars = word.chars();
        if let Some(first) = chars.next() {
            output.extend(first.to_uppercase());
            output.push_str(chars.as_str());
        }
    }
    if output.is_empty() {
        pick(ui, "Desconocido", "Unknown")
    } else {
        output
    }
}

#[cfg(test)]
mod tests {
    use super::{category, language, plural, source};
    use crate::presentation::UiLanguage;

    const ES: UiLanguage = UiLanguage::Spanish;
    const EN: UiLanguage = UiLanguage::English;

    #[test]
    fn labels_known_categories_and_falls_back() {
        assert_eq!(category(ES, "refran"), "Refrán");
        assert_eq!(category(ES, "cita"), "Cita literaria");
        assert_eq!(category(ES, "sentencia_popular"), "Sentencia Popular");
        assert_eq!(category(EN, "sentencia_popular"), "Sentencia Popular");
    }

    #[test]
    fn labels_languages_and_uppercases_unknown_codes() {
        assert_eq!(language(ES, "es"), "Español");
        assert_eq!(language(ES, "ca"), "Catalán");
        assert_eq!(language(ES, "ast"), "Asturiano");
        assert_eq!(language(ES, "xx"), "XX");
        assert_eq!(language(ES, ""), "Desconocido");
        assert_eq!(language(EN, "xx"), "XX");
        assert_eq!(language(EN, ""), "Unknown");
    }

    #[test]
    fn labels_sources_present_in_the_catalog() {
        assert_eq!(source(ES, "cervantes_virtual"), "Cervantes Virtual");
        assert_eq!(source(ES, "cvc"), "Centro Virtual Cervantes");
        assert_eq!(
            source(ES, "cvc_wayback"),
            "Centro Virtual Cervantes · Archivo"
        );
        assert_eq!(source(ES, "mi_archivo"), "Mi Archivo");
        assert_eq!(source(ES, ""), "Fuente desconocida");
        assert_eq!(source(EN, "mi_archivo"), "Mi Archivo");
        assert_eq!(source(EN, ""), "Unknown source");
    }

    #[test]
    fn separator_only_identifiers_fall_back_to_the_interface_language() {
        assert_eq!(source(ES, "___"), "Desconocido");
        assert_eq!(source(EN, "___"), "Unknown");
        assert_eq!(category(ES, "__"), "Desconocido");
        assert_eq!(category(EN, "__"), "Unknown");
    }

    #[test]
    fn plural_labels_cover_every_category() {
        for value in [
            "refran",
            "paremia",
            "proverbio",
            "dicho",
            "adivinanza",
            "cita",
        ] {
            assert_ne!(plural(ES, value), "ENTRADAS");
            assert_ne!(plural(EN, value), "ENTRIES");
        }
        assert_eq!(plural(ES, "otra"), "ENTRADAS");
        assert_eq!(plural(EN, "otra"), "ENTRIES");
    }
}
