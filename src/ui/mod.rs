// SPDX-License-Identifier: GPL-3.0-or-later
pub mod gtk_app;
pub mod tui_app;

use crate::{config::ThemeColor, database::Database};
use chrono::{Datelike, Duration as ChronoDuration, Local};
use ratatui::style::Color;
use std::collections::HashMap;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum HistoryRange {
    Days7,
    Months12,
    Years,
}

#[derive(Clone, Debug)]
pub struct HistoryBucket {
    pub label: String,
    pub connected_sec: u64,
    pub listening_sec: u64,
}

pub fn tui_theme_colors(theme: ThemeColor) -> (Color, Color) {
    match theme {
        ThemeColor::Emerald => (Color::Green, Color::Cyan),
        ThemeColor::Cyberpunk => (Color::Cyan, Color::Magenta),
        ThemeColor::Amber => (Color::Yellow, Color::LightMagenta),
        ThemeColor::Crimson => (Color::LightRed, Color::LightCyan),
    }
}

pub fn compute_history_buckets(db: &Database, range: HistoryRange) -> Vec<HistoryBucket> {
    let today = Local::now().date_naive();
    let mut keys_and_labels = Vec::new();

    match range {
        HistoryRange::Days7 => {
            for i in (0..7).rev() {
                let d = today - ChronoDuration::days(i);
                let key = d.format("%Y-%m-%d").to_string();
                let lbl = d.format("%d/%m").to_string();
                keys_and_labels.push((key, lbl));
            }
        }
        HistoryRange::Months12 => {
            let mut y = today.year();
            let mut m = today.month() as i32;
            let mut list = Vec::new();
            for _ in 0..12 {
                let key = format!("{:04}-{:02}", y, m);
                let lbl = format!("{:02}/{}", m, y % 100);
                list.push((key, lbl));
                m -= 1;
                if m < 1 {
                    m = 12;
                    y -= 1;
                }
            }
            list.reverse();
            keys_and_labels = list;
        }
        HistoryRange::Years => {
            let current_y = today.year();
            for y in (current_y - 4)..=current_y {
                let key = format!("{:04}", y);
                keys_and_labels.push((key.clone(), key));
            }
        }
    }

    let mut map_conn: HashMap<String, u64> = HashMap::new();
    let mut map_play: HashMap<String, u64> = HashMap::new();

    for dev in db.devices.values() {
        for sess in &dev.sessions {
            let prefix = match range {
                HistoryRange::Days7 => sess.start.get(0..10),
                HistoryRange::Months12 => sess.start.get(0..7),
                HistoryRange::Years => sess.start.get(0..4),
            };
            if let Some(k) = prefix {
                *map_conn.entry(k.to_string()).or_insert(0) += sess.connected_seconds;
                *map_play.entry(k.to_string()).or_insert(0) += sess.listening_seconds;
            }
        }
    }

    keys_and_labels
        .into_iter()
        .map(|(k, label)| HistoryBucket {
            label,
            connected_sec: *map_conn.get(&k).unwrap_or(&0),
            listening_sec: *map_play.get(&k).unwrap_or(&0),
        })
        .collect()
}
