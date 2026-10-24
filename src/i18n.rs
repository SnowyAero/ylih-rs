// SPDX-License-Identifier: GPL-3.0-or-later
use std::{collections::HashMap, env, fs, path::PathBuf};

const ES_PAK_EMBED: &str = include_str!("../locales/es.pak");
const EN_PAK_EMBED: &str = include_str!("../locales/en.pak");

#[derive(Clone, Debug)]
pub struct I18n {
    pub lang_code: String,
    map: HashMap<String, String>,
}

impl I18n {
    pub fn new(lang_code: &str) -> Self {
        let mut map = HashMap::new();
        // Cargar base embebida primero
        let base_raw = match lang_code {
            "en" => EN_PAK_EMBED,
            _ => ES_PAK_EMBED,
        };
        Self::parse_pak(base_raw, &mut map);

        // Permitir override si existe ~/.local/share/ylih/locales/<lang>.pak o /usr/share/ylih/locales/<lang>.pak
        for ext_path in Self::external_pak_paths(lang_code) {
            if let Ok(content) = fs::read_to_string(ext_path) {
                Self::parse_pak(&content, &mut map);
                break;
            }
        }

        Self {
            lang_code: lang_code.to_string(),
            map,
        }
    }

    fn external_pak_paths(lang: &str) -> Vec<PathBuf> {
        let home = env::var("HOME").unwrap_or_else(|_| ".".into());
        vec![
            PathBuf::from(home).join(format!(".local/share/ylih/locales/{}.pak", lang)),
            PathBuf::from(format!("/usr/share/ylih/locales/{}.pak", lang)),
        ]
    }

    fn parse_pak(raw: &str, map: &mut HashMap<String, String>) {
        for line in raw.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            if let Some((k, v)) = trimmed.split_once('=') {
                map.insert(k.trim().to_string(), v.trim().to_string());
            }
        }
    }

    pub fn t<'a>(&'a self, key: &'a str) -> &'a str {
        self.map.get(key).map(|s| s.as_str()).unwrap_or(key)
    }
}
