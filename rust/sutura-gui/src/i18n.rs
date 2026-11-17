#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Language {
    System,
    En,
    Tr,
}

impl Language {
    pub fn code(self) -> &'static str {
        match self {
            Language::En => "en",
            Language::Tr => "tr",
            Language::System => {
                let locale = sys_locale::get_locale().unwrap_or_default();
                if locale.to_ascii_lowercase().starts_with("tr") {
                    "tr"
                } else {
                    "en"
                }
            }
        }
    }

    pub fn label(self) -> String {
        match self {
            Language::System => rust_i18n::t!("app.lang_system").to_string(),
            Language::En => rust_i18n::t!("app.lang_english").to_string(),
            Language::Tr => rust_i18n::t!("app.lang_turkish").to_string(),
        }
    }
}

pub fn apply(language: Language) {
    rust_i18n::set_locale(language.code());
}
