// SPDX-License-Identifier: GPL-3.0-or-later
use crate::{
    config::{ChartType, Settings, ThemeColor},
    database::{write_debug_log, DeviceRecord, Session, TelemetryPoint, Tracker},
    telemetry::{format_short, format_time, is_systemd_enabled, set_systemd_service},
    ui::{compute_history_buckets, tui_theme_colors, HistoryRange},
};
use chrono::Local;
use crossterm::{
    event::{self, Event, KeyCode},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    symbols,
    text::{Line, Span},
    widgets::{
        canvas::{Canvas, Line as CanvasLine, Rectangle as CanvasRect},
        Block, Borders, Clear, List, ListItem, Paragraph, Tabs,
    },
    Terminal,
};
use std::{
    io::stdout,
    time::{Duration, Instant},
};

fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}

pub fn run_tui(mut tracker: Tracker) -> Result<(), Box<dyn std::error::Error>> {
    tracker.clean_orphan_sessions();
    tracker.tick(true);

    enable_raw_mode()?;
    let mut stdout = stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut last_tick = Instant::now();
    let mut active_tab: usize = 0;
    let mut hist_range = HistoryRange::Days7;
    let mut selected_session_idx: usize = 0;
    let mut show_modal = false;
    let mut settings_cursor: usize = 0;
    let mut draft_settings = tracker.db.settings.clone();
    let mut draft_systemd = is_systemd_enabled();
    let mut status_msg = String::from("[Tab] Pestaña/Tab | [1/2/3] Escala/Range | [Enter] Acción/Action");

    loop {
        let poll = Duration::from_secs(tracker.db.settings.poll_interval.max(1));
        if last_tick.elapsed() >= poll {
            tracker.tick(false);
            last_tick = Instant::now();
        }

        let mut flat_sessions: Vec<(String, DeviceRecord, Session)> = Vec::new();
        for (id, dev) in &tracker.db.devices {
            for sess in dev.sessions.iter().rev().take(8) {
                flat_sessions.push((id.clone(), dev.clone(), sess.clone()));
            }
        }
        if !flat_sessions.is_empty() && selected_session_idx >= flat_sessions.len() {
            selected_session_idx = flat_sessions.len() - 1;
        }

        terminal.draw(|f| {
            let i18n = &tracker.i18n;
            let s = &tracker.db.settings;
            let (col_primary, col_secondary) = tui_theme_colors(s.theme_color);

            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .margin(1)
                .constraints([
                    Constraint::Length(3),
                    Constraint::Min(10),
                    Constraint::Length(3),
                ])
                .split(f.size());

            let tab_titles = vec![
                Line::from(format!(" 🎧 1: {} ", i18n.t("tab.devices"))),
                Line::from(format!(" 📈 2: {} ", i18n.t("tab.history"))),
                Line::from(format!(" ⚙ 3: {} ", i18n.t("tab.settings"))),
            ];
            let tabs = Tabs::new(tab_titles)
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title(format!(" {} v2.2.0 ", i18n.t("app.title"))),
                )
                .select(active_tab)
                .style(Style::default().fg(Color::White))
                .highlight_style(
                    Style::default()
                        .fg(col_primary)
                        .add_modifier(Modifier::BOLD),
                );
            f.render_widget(tabs, chunks[0]);

            if active_tab == 0 {
                let mut items = Vec::new();
                for (id, dev) in &tracker.db.devices {
                    let status = if dev.currently_connected {
                        format!("🟢 {}", i18n.t("status.connected"))
                    } else {
                        format!("⚪ {}", i18n.t("status.disconnected"))
                    };
                    let codec = if s.show_codec && dev.currently_connected {
                        format!(" | {}", dev.current_codec.as_deref().unwrap_or("PCM/SBC"))
                    } else {
                        String::new()
                    };
                    let real_info = if !dev.real_name.is_empty() && dev.real_name != dev.name {
                        format!(" [{}]", dev.real_name)
                    } else {
                        String::new()
                    };

                    items.push(ListItem::new(Line::from(vec![Span::styled(
                        format!(
                            "▌ {} ({}) [{}]{} — {}{}",
                            dev.name, id, dev.conn_type, real_info, status, codec
                        ),
                        Style::default()
                            .fg(col_primary)
                            .add_modifier(Modifier::BOLD),
                    )])));

                    let active_sess = dev.sessions.last().filter(|x| x.status == "active");
                    let cur_conn_sec = active_sess.map(|x| x.connected_seconds).unwrap_or(0);
                    let cur_play_sec = active_sess.map(|x| x.listening_seconds).unwrap_or(0);

                    if s.only_playback == false {
                        let txt = if active_sess.is_some() {
                            format_time(cur_conn_sec, s.show_seconds)
                        } else {
                            format!("0m 00s ({})", i18n.t("dev.no_active_session"))
                        };
                        items.push(ListItem::new(format!(
                            "   ⏱ {:<38}: {}",
                            i18n.t("dev.conn_session"),
                            txt
                        )));
                    }

                    let play_txt = if active_sess.is_some() {
                        format_time(cur_play_sec, s.show_seconds)
                    } else {
                        "0m 00s".into()
                    };
                    items.push(ListItem::new(format!(
                        "   ▶ {:<38}: {}",
                        i18n.t("dev.play_session"),
                        play_txt
                    )));

                    if let Some(bat) = dev.current_battery.or_else(|| {
                        dev.sessions
                            .iter()
                            .rev()
                            .find_map(|x| x.battery_end.or(x.battery_start))
                    }) {
                        let filled = (bat as usize / 5).min(20);
                        let bar = format!(
                            "[{}{}] {}%",
                            "█".repeat(filled),
                            "░".repeat(20 - filled),
                            bat
                        );
                        items.push(ListItem::new(format!(
                            "   🔋 {:<38}: {}",
                            i18n.t("dev.battery"),
                            bar
                        )));
                    } else if dev.conn_type == "Bluetooth" {
                        items.push(ListItem::new(format!(
                            "   🔋 {:<38}: {}",
                            i18n.t("dev.battery"),
                            i18n.t("dev.bt_no_bat")
                        )));
                    } else {
                        items.push(ListItem::new(format!(
                            "   🔌 {:<38}: {}",
                            i18n.t("dev.power"),
                            i18n.t("dev.wired_no_bat")
                        )));
                    }

                    if let Some(act) = active_sess {
                        let vol = dev.current_volume.or(act.volume_avg).unwrap_or(0);
                        items.push(ListItem::new(format!(
                            "   🔊 {}: {}% | {}: {}",
                            i18n.t("dev.current_vol"),
                            vol,
                            i18n.t("dev.session_started"),
                            act.start.replace('T', " ")
                        )));
                    }

                    items.push(ListItem::new("─".repeat(82)));
                }

                let list = List::new(items)
                    .block(Block::default().borders(Borders::ALL).title(format!(
                        " {} ",
                        i18n.t("tab.devices")
                    )));
                f.render_widget(list, chunks[1]);
            } else if active_tab == 1 {
                let sub_chunks = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([Constraint::Length(11), Constraint::Min(8)])
                    .split(chunks[1]);

                let buckets = compute_history_buckets(&tracker.db, hist_range);
                let max_sec = buckets
                    .iter()
                    .map(|b| b.connected_sec.max(b.listening_sec))
                    .max()
                    .unwrap_or(60)
                    .max(60) as f64;
                let total_period_conn: u64 = buckets.iter().map(|b| b.connected_sec).sum();
                let total_period_play: u64 = buckets.iter().map(|b| b.listening_sec).sum();
                let avg_conn = total_period_conn / (buckets.len().max(1) as u64);

                let range_name = match hist_range {
                    HistoryRange::Days7 => i18n.t("hist.range_7d"),
                    HistoryRange::Months12 => i18n.t("hist.range_12m"),
                    HistoryRange::Years => i18n.t("hist.range_years"),
                };

                let n_pts = (buckets.len().saturating_sub(1)).max(1) as f64;
                let b_clone = buckets.clone();
                let chart_style = s.chart_type;

                let hist_canvas = Canvas::default()
                    .block(Block::default().borders(Borders::ALL).title(format!(
                        " {} ({}) — {}: {} | {}: {} | {}: {} ",
                        i18n.t("hist.graph_title"),
                        range_name,
                        i18n.t("hist.connected"),
                        format_short(total_period_conn),
                        i18n.t("hist.playing"),
                        format_short(total_period_play),
                        i18n.t("hist.avg"),
                        format_short(avg_conn)
                    )))
                    .marker(symbols::Marker::Braille)
                    .x_bounds([0.0, n_pts])
                    .y_bounds([0.0, max_sec])
                    .paint(move |ctx| match chart_style {
                        ChartType::Lines => {
                            for i in 0..b_clone.len().saturating_sub(1) {
                                ctx.draw(&CanvasLine {
                                    x1: i as f64,
                                    y1: b_clone[i].connected_sec as f64,
                                    x2: (i + 1) as f64,
                                    y2: b_clone[i + 1].connected_sec as f64,
                                    color: col_primary,
                                });
                                ctx.draw(&CanvasLine {
                                    x1: i as f64,
                                    y1: b_clone[i].listening_sec as f64,
                                    x2: (i + 1) as f64,
                                    y2: b_clone[i + 1].listening_sec as f64,
                                    color: col_secondary,
                                });
                            }
                        }
                        ChartType::Bars => {
                            for (i, b) in b_clone.iter().enumerate() {
                                let x = i as f64;
                                ctx.draw(&CanvasRect {
                                    x: (x - 0.18).max(0.0),
                                    y: 0.0,
                                    width: 0.16,
                                    height: b.connected_sec as f64,
                                    color: col_primary,
                                });
                                ctx.draw(&CanvasRect {
                                    x: x + 0.02,
                                    y: 0.0,
                                    width: 0.16,
                                    height: b.listening_sec as f64,
                                    color: col_secondary,
                                });
                            }
                        }
                        ChartType::Pie => {
                            let ratio = if total_period_conn > 0 {
                                (total_period_play as f64 / total_period_conn as f64).clamp(0.0, 1.0)
                            } else {
                                0.0
                            };
                            let split_x = n_pts * ratio;
                            ctx.draw(&CanvasRect {
                                x: 0.0,
                                y: max_sec * 0.25,
                                width: split_x,
                                height: max_sec * 0.5,
                                color: col_secondary,
                            });
                            ctx.draw(&CanvasRect {
                                x: split_x,
                                y: max_sec * 0.25,
                                width: (n_pts - split_x).max(0.0),
                                height: max_sec * 0.5,
                                color: col_primary,
                            });
                        }
                    });
                f.render_widget(hist_canvas, sub_chunks[0]);

                let mut items = Vec::new();
                let mut current_flat_idx = 0;

                let labels_line: String = buckets
                    .iter()
                    .map(|b| format!("{}: {}", b.label, format_short(b.connected_sec)))
                    .collect::<Vec<_>>()
                    .join("  |  ");
                items.push(ListItem::new(Line::from(Span::styled(
                    format!(" 📅 {}", labels_line),
                    Style::default().fg(Color::Yellow),
                ))));
                items.push(ListItem::new("─".repeat(82)));

                for (id, dev) in &tracker.db.devices {
                    items.push(ListItem::new(Line::from(Span::styled(
                        format!(
                            "▌ {} ({}) [{}] — {}",
                            dev.name,
                            id,
                            dev.conn_type,
                            i18n.t("hist.total_header")
                        ),
                        Style::default()
                            .fg(col_primary)
                            .add_modifier(Modifier::BOLD),
                    ))));

                    if s.only_playback == false {
                        items.push(ListItem::new(format!(
                            "   ⏱ {:<28}: {}",
                            i18n.t("hist.total_connected"),
                            format_time(dev.lifetime_connected_seconds, s.show_seconds)
                        )));
                    }
                    items.push(ListItem::new(format!(
                        "   ▶ {:<28}: {}",
                        i18n.t("hist.total_playing"),
                        format_time(dev.lifetime_listening_seconds, s.show_seconds)
                    )));

                    for sess in dev.sessions.iter().rev().take(8) {
                        let is_sel = current_flat_idx == selected_session_idx;
                        let prefix = if is_sel { "     ➤ [Enter] " } else { "       • " };
                        let b_str = match (sess.battery_start, sess.battery_end) {
                            (Some(b1), Some(b2)) => format!("Bat: {}%➔{}%", b1, b2),
                            _ => "-".to_string(),
                        };
                        let v_str = sess
                            .volume_avg
                            .map(|v| {
                                format!(
                                    " | Vol: {}%{}",
                                    v,
                                    if sess.volume_increased { " ↑" } else { "" }
                                )
                            })
                            .unwrap_or_default();

                        let row_txt = format!(
                            "{}{} | {}: {} | {}: {} | {}{} [{}]",
                            prefix,
                            sess.start.replace('T', " "),
                            i18n.t("hist.connected"),
                            sess.connected_human,
                            i18n.t("hist.playing"),
                            sess.listening_human,
                            b_str,
                            v_str,
                            sess.status
                        );
                        let style = if is_sel {
                            Style::default()
                                .fg(Color::Black)
                                .bg(col_primary)
                                .add_modifier(Modifier::BOLD)
                        } else {
                            Style::default()
                        };
                        items.push(ListItem::new(Line::from(Span::styled(row_txt, style))));
                        current_flat_idx += 1;
                    }
                    items.push(ListItem::new("─".repeat(82)));
                }

                let hist_list = List::new(items).block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title(format!(" {} ([1] 7D | [2] 12M | [3] Years) ", i18n.t("tab.history"))),
                );
                f.render_widget(hist_list, sub_chunks[1]);
            } else {
                let chart_lbl = match draft_settings.chart_type {
                    ChartType::Lines => i18n.t("set.chart_lines"),
                    ChartType::Bars => i18n.t("set.chart_bars"),
                    ChartType::Pie => i18n.t("set.chart_pie"),
                };
                let color_lbl = match draft_settings.theme_color {
                    ThemeColor::Emerald => i18n.t("set.color_emerald"),
                    ThemeColor::Cyberpunk => i18n.t("set.color_cyber"),
                    ThemeColor::Amber => i18n.t("set.color_amber"),
                    ThemeColor::Crimson => i18n.t("set.color_crimson"),
                };
                let lang_lbl = if draft_settings.language == "en" {
                    "English (en.pak)"
                } else {
                    "Español (es.pak)"
                };

                let rows = vec![
                    format!("{:<44} < {} s >", i18n.t("set.poll_interval"), draft_settings.poll_interval),
                    format!("{:<44} [{}]", i18n.t("set.show_seconds"), draft_settings.show_seconds),
                    format!("{:<44} [{}]", i18n.t("set.show_codec"), draft_settings.show_codec),
                    format!("{:<44} [{}]", i18n.t("set.only_playback"), draft_settings.only_playback),
                    format!("{:<44} [{}]", i18n.t("set.track_wired"), draft_settings.track_wired_usb),
                    format!("{:<44} [{}]", i18n.t("set.systemd"), draft_systemd),
                    format!("{:<44} < {} >", i18n.t("set.language"), lang_lbl),
                    format!("{:<44} < {} >", i18n.t("set.chart_type"), chart_lbl),
                    format!("{:<44} < {} >", i18n.t("set.theme_color"), color_lbl),
                    "──────────────────────────────────────────────────────────────".to_string(),
                    format!("💾 [ {} ]", i18n.t("set.btn_save")),
                    format!("↩  [ {} ]", i18n.t("set.btn_revert")),
                    format!("🔄 [ {} ]", i18n.t("set.btn_default")),
                    format!("🛠  [ {} ]", i18n.t("set.btn_debug")),
                ];

                let mut list_items = Vec::new();
                for (i, r) in rows.iter().enumerate() {
                    let is_sel = i == settings_cursor;
                    let style = if is_sel {
                        Style::default()
                            .fg(Color::Black)
                            .bg(col_primary)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default()
                    };
                    let prefix = if is_sel { " ➤ " } else { "   " };
                    list_items.push(ListItem::new(Line::from(Span::styled(
                        format!("{}{}", prefix, r),
                        style,
                    ))));
                }

                let settings_list = List::new(list_items).block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title(format!(" {} ", i18n.t("tab.settings"))),
                );
                f.render_widget(settings_list, chunks[1]);
            }

            let footer = Paragraph::new(format!(" {} | [q] Quit/Salir", status_msg))
                .style(Style::default().fg(Color::Gray))
                .block(Block::default().borders(Borders::ALL));
            f.render_widget(footer, chunks[2]);

            if show_modal {
                if let Some((mac, dev, sess)) = flat_sessions.get(selected_session_idx) {
                    let area = centered_rect(84, 82, f.size());
                    f.render_widget(Clear, area);

                    let modal_chunks = Layout::default()
                        .direction(Direction::Vertical)
                        .margin(1)
                        .constraints([Constraint::Length(8), Constraint::Min(10)])
                        .split(area);

                    let outer_block = Block::default()
                        .borders(Borders::ALL)
                        .title(format!(" 📊 {} ([Esc/Enter] {}) ", i18n.t("dlg.title"), i18n.t("dlg.close")))
                        .style(Style::default().bg(Color::Black).fg(Color::White));
                    f.render_widget(outer_block, area);

                    let real_model = if dev.real_name.is_empty() {
                        &dev.name
                    } else {
                        &dev.real_name
                    };
                    let v_start = sess.volume_start.map(|v| format!("{}%", v)).unwrap_or_else(|| "N/D".into());
                    let v_avg = sess.volume_avg.map(|v| format!("{}%", v)).unwrap_or_else(|| "N/D".into());
                    let v_max = sess.volume_max.map(|v| format!("{}%", v)).unwrap_or_else(|| "N/D".into());
                    let v_inc = if sess.volume_increased {
                        i18n.t("dlg.vol_inc_yes")
                    } else {
                        i18n.t("dlg.vol_inc_no")
                    };
                    let bat_txt = match (sess.battery_start, sess.battery_end) {
                        (Some(b1), Some(b2)) => format!("{}% ➔ {}%", b1, b2),
                        _ => i18n.t("dev.wired_no_bat").into(),
                    };

                    let info_text = vec![
                        Line::from(format!(
                            " 🎧 {}: {} | {}: {} | {}: {}",
                            i18n.t("dlg.alias"),
                            dev.name,
                            i18n.t("dlg.model"),
                            real_model,
                            i18n.t("dlg.type"),
                            dev.conn_type
                        )),
                        Line::from(format!(
                            " 📡 {}: {} | {}: {}",
                            i18n.t("dlg.address"),
                            mac,
                            i18n.t("dlg.codec"),
                            sess.codec.as_deref().unwrap_or("PCM/SBC")
                        )),
                        Line::from(format!(
                            " 🔊 {}: {}: {} | {}: {} | {}: {} | {} {}",
                            i18n.t("dlg.vol_session"),
                            i18n.t("dlg.vol_init"),
                            v_start,
                            i18n.t("dlg.vol_avg"),
                            v_avg,
                            i18n.t("dlg.vol_max"),
                            v_max,
                            i18n.t("dlg.vol_inc_q"),
                            v_inc
                        )),
                        Line::from(format!(
                            " 🔋 {}: {} | {}: {} | {}: {}",
                            i18n.t("hist.connected"),
                            sess.connected_human,
                            i18n.t("hist.playing"),
                            sess.listening_human,
                            i18n.t("dev.battery"),
                            bat_txt
                        )),
                    ];
                    let info_para = Paragraph::new(info_text).block(
                        Block::default()
                            .borders(Borders::ALL)
                            .title(format!(" {} ", i18n.t("dlg.telemetry_frame"))),
                    );
                    f.render_widget(info_para, modal_chunks[0]);

                    let mut pts = sess.samples.clone();
                    if pts.is_empty() {
                        pts.push(TelemetryPoint {
                            t_sec: 0,
                            battery: sess.battery_start,
                            volume: sess.volume_start,
                        });
                        pts.push(TelemetryPoint {
                            t_sec: sess.connected_seconds.max(1),
                            battery: sess.battery_end,
                            volume: sess.volume_max.or(sess.volume_avg),
                        });
                    } else if pts.len() == 1 {
                        let mut p2 = pts[0].clone();
                        p2.t_sec = sess.connected_seconds.max(1);
                        pts.push(p2);
                    }
                    let max_t = pts.last().map(|p| p.t_sec).unwrap_or(1).max(1) as f64;

                    let canvas = Canvas::default()
                        .block(
                            Block::default()
                                .borders(Borders::ALL)
                                .title(format!(" {} ", i18n.t("dlg.graph_title"))),
                        )
                        .marker(symbols::Marker::Braille)
                        .x_bounds([0.0, max_t])
                        .y_bounds([0.0, 100.0])
                        .paint(|ctx| {
                            for w in pts.windows(2) {
                                if let (Some(b1), Some(b2)) = (w[0].battery, w[1].battery) {
                                    ctx.draw(&CanvasLine {
                                        x1: w[0].t_sec as f64,
                                        y1: b1.min(100) as f64,
                                        x2: w[1].t_sec as f64,
                                        y2: b2.min(100) as f64,
                                        color: col_primary,
                                    });
                                }
                                if let (Some(v1), Some(v2)) = (w[0].volume, w[1].volume) {
                                    ctx.draw(&CanvasLine {
                                        x1: w[0].t_sec as f64,
                                        y1: v1.min(100) as f64,
                                        x2: w[1].t_sec as f64,
                                        y2: v2.min(100) as f64,
                                        color: col_secondary,
                                    });
                                }
                            }
                        });
                    f.render_widget(canvas, modal_chunks[1]);
                }
            }
        })?;

        if event::poll(Duration::from_millis(200))? {
            if let Event::Key(key) = event::read()? {
                if show_modal {
                    match key.code {
                        KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') => show_modal = false,
                        _ => {}
                    }
                    continue;
                }

                match key.code {
                    KeyCode::Char('q') => break,
                    KeyCode::Tab => active_tab = (active_tab + 1) % 3,
                    KeyCode::Char('1') if active_tab == 1 => hist_range = HistoryRange::Days7,
                    KeyCode::Char('2') if active_tab == 1 => hist_range = HistoryRange::Months12,
                    KeyCode::Char('3') if active_tab == 1 => hist_range = HistoryRange::Years,
                    KeyCode::Up | KeyCode::Char('k') => {
                        if active_tab == 1 {
                            selected_session_idx = selected_session_idx.saturating_sub(1);
                        } else if active_tab == 2 {
                            settings_cursor = settings_cursor.saturating_sub(1);
                            if settings_cursor == 9 {
                                settings_cursor = 8;
                            }
                        }
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        if active_tab == 1 && !flat_sessions.is_empty() {
                            selected_session_idx = (selected_session_idx + 1).min(flat_sessions.len() - 1);
                        } else if active_tab == 2 {
                            settings_cursor = (settings_cursor + 1).min(13);
                            if settings_cursor == 9 {
                                settings_cursor = 10;
                            }
                        }
                    }
                    KeyCode::Enter | KeyCode::Char(' ') => {
                        if active_tab == 1 && !flat_sessions.is_empty() {
                            show_modal = true;
                        } else if active_tab == 2 {
                            match settings_cursor {
                                0 => draft_settings.poll_interval = (draft_settings.poll_interval % 60) + 1,
                                1 => draft_settings.show_seconds = !draft_settings.show_seconds,
                                2 => draft_settings.show_codec = !draft_settings.show_codec,
                                3 => draft_settings.only_playback = !draft_settings.only_playback,
                                4 => draft_settings.track_wired_usb = !draft_settings.track_wired_usb,
                                5 => draft_systemd = !draft_systemd,
                                6 => {
                                    draft_settings.language = if draft_settings.language == "es" {
                                        "en".into()
                                    } else {
                                        "es".into()
                                    };
                                }
                                7 => draft_settings.chart_type = draft_settings.chart_type.next(),
                                8 => draft_settings.theme_color = draft_settings.theme_color.next(),
                                10 => {
                                    tracker.db.settings = draft_settings.clone();
                                    tracker.reload_i18n();
                                    tracker.save_db();
                                    set_systemd_service(draft_systemd);
                                    tracker.tick(false);
                                    status_msg = tracker.i18n.t("set.saved_ok").to_string();
                                }
                                11 => {
                                    draft_settings = tracker.db.settings.clone();
                                    draft_systemd = is_systemd_enabled();
                                    status_msg = tracker.i18n.t("set.reverted_ok").to_string();
                                }
                                12 => {
                                    draft_settings = Settings::default();
                                    tracker.db.settings = draft_settings.clone();
                                    tracker.reload_i18n();
                                    tracker.save_db();
                                    status_msg = tracker.i18n.t("set.default_ok").to_string();
                                }
                                13 => {
                                    let out_file = crate::database::applogs_dir_path().join(format!(
                                        "ylih_debug_{}.log",
                                        Local::now().format("%Y%m%d_%H%M%S")
                                    ));
                                    if write_debug_log(&out_file).is_ok() {
                                        status_msg = format!("🛠 {}", out_file.display());
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    Ok(())
}
