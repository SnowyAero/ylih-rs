// SPDX-License-Identifier: GPL-3.0-or-later
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChartType {
    Lines,
    Bars,
    Pie,
}

impl Default for ChartType {
    fn default() -> Self {
        ChartType::Lines
    }
}

impl ChartType {
    pub fn next(self) -> Self {
        match self {
            ChartType::Lines => ChartType::Bars,
            ChartType::Bars => ChartType::Pie,
            ChartType::Pie => ChartType::Lines,
        }
    }
    pub fn from_index(idx: u32) -> Self {
        match idx {
            1 => ChartType::Bars,
            2 => ChartType::Pie,
            _ => ChartType::Lines,
        }
    }
    pub fn to_index(self) -> u32 {
        match self {
            ChartType::Lines => 0,
            ChartType::Bars => 1,
            ChartType::Pie => 2,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThemeColor {
    Emerald,
    Cyberpunk,
    Amber,
    Crimson,
}

impl Default for ThemeColor {
    fn default() -> Self {
        ThemeColor::Emerald
    }
}

impl ThemeColor {
    pub fn next(self) -> Self {
        match self {
            ThemeColor::Emerald => ThemeColor::Cyberpunk,
            ThemeColor::Cyberpunk => ThemeColor::Amber,
            ThemeColor::Amber => ThemeColor::Crimson,
            ThemeColor::Crimson => ThemeColor::Emerald,
        }
    }
    pub fn from_index(idx: u32) -> Self {
        match idx {
            1 => ThemeColor::Cyberpunk,
            2 => ThemeColor::Amber,
            3 => ThemeColor::Crimson,
            _ => ThemeColor::Emerald,
        }
    }
    pub fn to_index(self) -> u32 {
        match self {
            ThemeColor::Emerald => 0,
            ThemeColor::Cyberpunk => 1,
            ThemeColor::Amber => 2,
            ThemeColor::Crimson => 3,
        }
    }
    // Devuelve (RGB Principal Conectado/Batería, RGB Secundario Reproduciendo/Volumen, Hex1, Hex2)
    pub fn palette_rgb(self) -> ((f64, f64, f64), (f64, f64, f64), &'static str, &'static str) {
        match self {
            ThemeColor::Emerald => ((0.30, 0.85, 0.45), (0.35, 0.70, 0.98), "#4ade80", "#60a5fa"),
            ThemeColor::Cyberpunk => ((0.00, 0.95, 0.95), (0.95, 0.30, 0.85), "#00f2f2", "#f472b6"),
            ThemeColor::Amber => ((0.98, 0.75, 0.15), (0.68, 0.45, 0.98), "#facc15", "#a855f7"),
            ThemeColor::Crimson => ((0.96, 0.32, 0.40), (0.20, 0.85, 0.75), "#f43f5e", "#2dd4bf"),
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Settings {
    #[serde(default = "default_poll")]
    pub poll_interval: u64,
    #[serde(default = "default_true")]
    pub show_seconds: bool,
    #[serde(default)]
    pub only_playback: bool,
    #[serde(default = "default_true")]
    pub show_codec: bool,
    #[serde(default = "default_true")]
    pub track_wired_usb: bool,
    #[serde(default = "default_lang")]
    pub language: String,
    #[serde(default)]
    pub chart_type: ChartType,
    #[serde(default)]
    pub theme_color: ThemeColor,
}

fn default_poll() -> u64 { 5 }
fn default_true() -> bool { true }
fn default_lang() -> String { "es".into() }

impl Default for Settings {
    fn default() -> Self {
        Self {
            poll_interval: 5,
            show_seconds: true,
            only_playback: false,
            show_codec: true,
            track_wired_usb: true,
            language: "es".into(),
            chart_type: ChartType::Lines,
            theme_color: ThemeColor::Emerald,
        }
    }
}
