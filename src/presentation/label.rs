//! Human-readable labels for stored identifiers.

/// Readable category name.
pub fn category(value: &str) -> String {
    match value {
        "refran" => "Refrán".to_owned(),
        "paremia" => "Paremia".to_owned(),
        "proverbio" => "Proverbio".to_owned(),
        "dicho" => "Dicho".to_owned(),
        "adivinanza" => "Adivinanza".to_owned(),
        "cita" => "Cita literaria".to_owned(),
        other => humanize(other),
    }
}

/// Plural label used by collection headers.
pub fn plural(value: &str) -> &'static str {
    match value {
        "refran" => "REFRANES",
        "paremia" => "PAREMIAS",
        "proverbio" => "PROVERBIOS",
        "dicho" => "DICHOS",
        "adivinanza" => "ADIVINANZAS",
        "cita" => "CITAS LITERARIAS",
        _ => "ENTRADAS",
    }
}

/// Readable language name.
pub fn language(code: &str) -> String {
    match code {
        "es" => "Español".to_owned(),
        "ca" => "Catalán".to_owned(),
        "gl" => "Gallego".to_owned(),
        "eu" => "Euskera".to_owned(),
        "en" => "Inglés".to_owned(),
        "fr" => "Francés".to_owned(),
        "de" => "Alemán".to_owned(),
        "it" => "Italiano".to_owned(),
        "pt" => "Portugués".to_owned(),
        "ast" => "Asturiano".to_owned(),
        "ar" => "Árabe".to_owned(),
        "he" => "Hebreo".to_owned(),
        "zh" => "Chino".to_owned(),
        "ja" => "Japonés".to_owned(),
        "" => "Desconocido".to_owned(),
        other => other.to_uppercase(),
    }
}

/// Readable source name. Unknown values are humanised, never shown as raw identifiers.
pub fn source(value: &str) -> String {
    match value {
        "cervantes_virtual" => "Cervantes Virtual".to_owned(),
        "cvc" => "Centro Virtual Cervantes".to_owned(),
        "cvc_wayback" => "Centro Virtual Cervantes · Archivo".to_owned(),
        "" => "Fuente desconocida".to_owned(),
        other => humanize(other),
    }
}

fn humanize(value: &str) -> String {
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
        "Desconocido".to_owned()
    } else {
        output
    }
}

#[cfg(test)]
mod tests {
    use super::{category, language, plural, source};

    #[test]
    fn labels_known_categories_and_falls_back() {
        assert_eq!(category("refran"), "Refrán");
        assert_eq!(category("cita"), "Cita literaria");
        assert_eq!(category("sentencia_popular"), "Sentencia Popular");
    }

    #[test]
    fn labels_languages_and_uppercases_unknown_codes() {
        assert_eq!(language("es"), "Español");
        assert_eq!(language("ca"), "Catalán");
        assert_eq!(language("ast"), "Asturiano");
        assert_eq!(language("xx"), "XX");
        assert_eq!(language(""), "Desconocido");
    }

    #[test]
    fn labels_sources_present_in_the_catalog() {
        assert_eq!(source("cervantes_virtual"), "Cervantes Virtual");
        assert_eq!(source("cvc"), "Centro Virtual Cervantes");
        assert_eq!(source("cvc_wayback"), "Centro Virtual Cervantes · Archivo");
        assert_eq!(source("mi_archivo"), "Mi Archivo");
        assert_eq!(source(""), "Fuente desconocida");
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
            assert_ne!(plural(value), "ENTRADAS");
        }
        assert_eq!(plural("otra"), "ENTRADAS");
    }
}
