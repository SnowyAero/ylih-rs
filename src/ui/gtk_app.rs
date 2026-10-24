// SPDX-License-Identifier: GPL-3.0-or-later
use crate::{
    config::{ChartType, Settings, ThemeColor},
    database::{write_debug_log, DeviceRecord, Session, TelemetryPoint, Tracker},
    i18n::I18n,
    telemetry::{format_short, format_time, is_systemd_enabled, set_systemd_service},
    ui::{compute_history_buckets, HistoryRange},
};
use chrono::Local;
use gtk::prelude::*;
use std::{cell::RefCell, collections::HashSet, rc::Rc, time::Duration};

fn icon_label_row(icon_name: &str, markup: &str) -> gtk::Box {
    let b = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let img = gtk::Image::from_icon_name(Some(icon_name), gtk::IconSize::Menu);
    let lbl = gtk::Label::new(None);
    lbl.set_markup(markup);
    lbl.set_xalign(0.0);
    b.pack_start(&img, false, false, 0);
    b.pack_start(&lbl, true, true, 0);
    b
}

fn show_session_details_dialog(
    parent: &gtk::Window,
    mac: &str,
    dev: &DeviceRecord,
    sess: &Session,
    i18n: &I18n,
    theme: ThemeColor,
) {
    let dlg = gtk::Dialog::new();
    dlg.set_title(i18n.t("dlg.title"));
    dlg.set_transient_for(Some(parent));
    dlg.set_modal(true);
    dlg.add_button(i18n.t("dlg.close"), gtk::ResponseType::Close);
    dlg.set_default_size(580, 450);
    dlg.set_border_width(12);

    let area = dlg.content_area();
    area.set_spacing(10);

    let real_model = if dev.real_name.is_empty() {
        &dev.name
    } else {
        &dev.real_name
    };

    let info_frame = gtk::Frame::new(Some(i18n.t("dlg.telemetry_frame")));
    let info_box = gtk::Box::new(gtk::Orientation::Vertical, 6);
    info_box.set_border_width(10);
    info_frame.add(&info_box);

    info_box.pack_start(
        &icon_label_row(
            "audio-headphones-symbolic",
            &format!(
                "<b>{}:</b> {}   |   <b>{}:</b> {}   |   <b>{}:</b> {}",
                i18n.t("dlg.alias"),
                glib::markup_escape_text(&dev.name),
                i18n.t("dlg.model"),
                glib::markup_escape_text(real_model),
                i18n.t("dlg.type"),
                glib::markup_escape_text(&dev.conn_type)
            ),
        ),
        false,
        false,
        0,
    );

    info_box.pack_start(
        &icon_label_row(
            "network-bluetooth-symbolic",
            &format!(
                "<b>{}:</b> <tt>{}</tt>   |   <b>{}:</b> {}",
                i18n.t("dlg.address"),
                glib::markup_escape_text(mac),
                i18n.t("dlg.codec"),
                sess.codec.as_deref().unwrap_or("PCM / AAC / SBC")
            ),
        ),
        false,
        false,
        0,
    );

    let v_start = sess
        .volume_start
        .map(|v| format!("{}%", v))
        .unwrap_or_else(|| "N/D".into());
    let v_avg = sess
        .volume_avg
        .map(|v| format!("{}%", v))
        .unwrap_or_else(|| "N/D".into());
    let v_max = sess
        .volume_max
        .map(|v| format!("{}%", v))
        .unwrap_or_else(|| "N/D".into());
    let v_inc_txt = if sess.volume_increased {
        i18n.t("dlg.vol_inc_yes")
    } else {
        i18n.t("dlg.vol_inc_no")
    };

    info_box.pack_start(
        &icon_label_row(
            "multimedia-volume-control-symbolic",
            &format!(
                "<b>{}:</b> {}: <b>{}</b>  |  {}: <b>{}</b>  |  {}: <b>{}</b>\n<b>{}</b> {}",
                i18n.t("dlg.vol_session"),
                i18n.t("dlg.vol_init"),
                v_start,
                i18n.t("dlg.vol_avg"),
                v_avg,
                i18n.t("dlg.vol_max"),
                v_max,
                i18n.t("dlg.vol_inc_q"),
                v_inc_txt
            ),
        ),
        false,
        false,
        0,
    );

    let bat_summary = match (sess.battery_start, sess.battery_end) {
        (Some(b1), Some(b2)) => format!("{}% ➔ {}%", b1, b2),
        _ => i18n.t("dev.wired_no_bat").into(),
    };

    info_box.pack_start(
        &icon_label_row(
            "battery-good-symbolic",
            &format!(
                "<b>{}:</b> {}   |   <b>{}:</b> {}   |   <b>{}:</b> {}",
                i18n.t("hist.connected"),
                sess.connected_human,
                i18n.t("hist.playing"),
                sess.listening_human,
                i18n.t("dev.battery"),
                bat_summary
            ),
        ),
        false,
        false,
        0,
    );

    area.pack_start(&info_frame, false, false, 0);

    let graph_frame = gtk::Frame::new(Some(i18n.t("dlg.graph_title")));
    let drawing = gtk::DrawingArea::new();
    drawing.set_size_request(520, 220);
    graph_frame.add(&drawing);
    area.pack_start(&graph_frame, true, true, 0);

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
        p2.battery = sess.battery_end.or(p2.battery);
        pts.push(p2);
    }

    let start_txt = i18n.t("dlg.start").to_string();
    let end_prefix = i18n.t("dlg.end").to_string();
    let (rgb1, rgb2, _, _) = theme.palette_rgb();

    drawing.connect_draw(move |widget, cr| {
        let w = widget.allocated_width() as f64;
        let h = widget.allocated_height() as f64;
        let pad_l = 42.0;
        let pad_r = 20.0;
        let pad_t = 18.0;
        let pad_b = 28.0;
        let gw = (w - pad_l - pad_r).max(10.0);
        let gh = (h - pad_t - pad_b).max(10.0);

        cr.set_source_rgb(0.11, 0.12, 0.14);
        let _ = cr.paint();

        cr.set_line_width(1.0);
        for step in [0, 25, 50, 75, 100] {
            let y = pad_t + gh * (1.0 - (step as f64 / 100.0));
            cr.set_source_rgba(1.0, 1.0, 1.0, 0.12);
            cr.move_to(pad_l, y);
            cr.line_to(pad_l + gw, y);
            let _ = cr.stroke();

            cr.set_source_rgba(0.85, 0.85, 0.85, 0.85);
            cr.move_to(6.0, y + 4.0);
            let _ = cr.show_text(&format!("{}%", step));
        }

        let max_t = pts.last().map(|p| p.t_sec).unwrap_or(1).max(1) as f64;

        let bat_pts: Vec<(f64, f64)> = pts
            .iter()
            .filter_map(|p| {
                p.battery.map(|b| {
                    let x = pad_l + (p.t_sec as f64 / max_t) * gw;
                    let y = pad_t + gh * (1.0 - (b.min(100) as f64 / 100.0));
                    (x, y)
                })
            })
            .collect();

        if !bat_pts.is_empty() {
            cr.set_source_rgb(rgb1.0, rgb1.1, rgb1.2);
            cr.set_line_width(2.5);
            for (i, &(x, y)) in bat_pts.iter().enumerate() {
                if i == 0 {
                    cr.move_to(x, y);
                } else {
                    cr.line_to(x, y);
                }
            }
            let _ = cr.stroke();
        }

        let vol_pts: Vec<(f64, f64)> = pts
            .iter()
            .filter_map(|p| {
                p.volume.map(|v| {
                    let x = pad_l + (p.t_sec as f64 / max_t) * gw;
                    let y = pad_t + gh * (1.0 - (v.min(100) as f64 / 100.0));
                    (x, y)
                })
            })
            .collect();

        if !vol_pts.is_empty() {
            cr.set_source_rgb(rgb2.0, rgb2.1, rgb2.2);
            cr.set_line_width(2.0);
            for (i, &(x, y)) in vol_pts.iter().enumerate() {
                if i == 0 {
                    cr.move_to(x, y);
                } else {
                    cr.line_to(x, y);
                }
            }
            let _ = cr.stroke();
        }

        cr.set_source_rgba(0.85, 0.85, 0.85, 0.85);
        cr.move_to(pad_l, h - 8.0);
        let _ = cr.show_text(&start_txt);
        let end_lbl = format!("{} ({})", end_prefix, format_short(max_t as u64));
        cr.move_to((pad_l + gw - 75.0).max(pad_l), h - 8.0);
        let _ = cr.show_text(&end_lbl);

        glib::Propagation::Proceed
    });

    dlg.show_all();
    let _ = dlg.run();
    unsafe {
        dlg.destroy();
    }
}

pub fn run_gtk(tracker: Tracker) {
    gtk::init().expect("Error inicializando GTK3");
    let tracker = Rc::new(RefCell::new(tracker));
    let hist_range = Rc::new(RefCell::new(HistoryRange::Days7));
    let expanded_devs: Rc<RefCell<HashSet<String>>> = Rc::new(RefCell::new(HashSet::new()));

    let win = gtk::Window::new(gtk::WindowType::Toplevel);
    win.set_title(tracker.borrow().i18n.t("app.title"));
    win.set_default_size(720, 640);
    win.set_border_width(10);
    win.set_icon_name(Some("audio-headphones"));

    let notebook = gtk::Notebook::new();
    win.add(&notebook);

    // PESTAÑA 1
    let tab_monitor = gtk::Box::new(gtk::Orientation::Vertical, 8);
    tab_monitor.set_border_width(10);
    let tab1_hdr = icon_label_row(
        "audio-headphones-symbolic",
        tracker.borrow().i18n.t("tab.devices"),
    );
    tab1_hdr.show_all();
    notebook.append_page(&tab_monitor, Some(&tab1_hdr));

    let scroll_live = gtk::ScrolledWindow::new(gtk::Adjustment::NONE, gtk::Adjustment::NONE);
    scroll_live.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    tab_monitor.pack_start(&scroll_live, true, true, 0);

    let live_box = gtk::Box::new(gtk::Orientation::Vertical, 12);
    scroll_live.add(&live_box);

    // PESTAÑA 2
    let tab_history = gtk::Box::new(gtk::Orientation::Vertical, 10);
    tab_history.set_border_width(10);
    let tab2_hdr = icon_label_row(
        "utilities-system-monitor-symbolic",
        tracker.borrow().i18n.t("tab.history"),
    );
    tab2_hdr.show_all();
    notebook.append_page(&tab_history, Some(&tab2_hdr));

    let range_bar = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let lbl_range_title = icon_label_row(
        "x-office-calendar-symbolic",
        &format!("<b>{}</b>", tracker.borrow().i18n.t("hist.scale_label")),
    );
    range_bar.pack_start(&lbl_range_title, false, false, 0);

    let btn_7d = gtk::Button::with_label(tracker.borrow().i18n.t("hist.range_7d"));
    let btn_12m = gtk::Button::with_label(tracker.borrow().i18n.t("hist.range_12m"));
    let btn_yr = gtk::Button::with_label(tracker.borrow().i18n.t("hist.range_years"));
    range_bar.pack_end(&btn_yr, false, false, 0);
    range_bar.pack_end(&btn_12m, false, false, 0);
    range_bar.pack_end(&btn_7d, false, false, 0);
    tab_history.pack_start(&range_bar, false, false, 0);

    let hist_graph_frame = gtk::Frame::new(Some(tracker.borrow().i18n.t("hist.graph_title")));
    let hist_graph_vbox = gtk::Box::new(gtk::Orientation::Vertical, 6);
    hist_graph_vbox.set_border_width(8);
    hist_graph_frame.add(&hist_graph_vbox);

    let hist_drawing = gtk::DrawingArea::new();
    hist_drawing.set_size_request(620, 200);
    hist_graph_vbox.pack_start(&hist_drawing, false, false, 0);

    let lbl_hist_summary = gtk::Label::new(None);
    lbl_hist_summary.set_xalign(0.0);
    hist_graph_vbox.pack_start(&lbl_hist_summary, false, false, 0);

    tab_history.pack_start(&hist_graph_frame, false, false, 0);

    let scroll_hist = gtk::ScrolledWindow::new(gtk::Adjustment::NONE, gtk::Adjustment::NONE);
    scroll_hist.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    tab_history.pack_start(&scroll_hist, true, true, 0);

    let hist_devices_box = gtk::Box::new(gtk::Orientation::Vertical, 12);
    scroll_hist.add(&hist_devices_box);

    // PESTAÑA 3: AJUSTES (Con Selectores de Idioma, Tipo de Gráfica y Tema de Color)
    let tab_settings = gtk::Box::new(gtk::Orientation::Vertical, 10);
    tab_settings.set_border_width(14);
    let tab3_hdr = icon_label_row(
        "preferences-system-symbolic",
        tracker.borrow().i18n.t("tab.settings"),
    );
    tab3_hdr.show_all();
    notebook.append_page(&tab_settings, Some(&tab3_hdr));

    let grid = gtk::Grid::new();
    grid.set_column_spacing(20);
    grid.set_row_spacing(10);
    tab_settings.pack_start(&grid, false, false, 0);

    let s = tracker.borrow().db.settings.clone();
    let i18n_init = tracker.borrow().i18n.clone();

    let row_poll = icon_label_row(
        "preferences-system-time-symbolic",
        &format!("<b>{}</b>", i18n_init.t("set.poll_interval")),
    );
    let adj = gtk::Adjustment::new(s.poll_interval as f64, 1.0, 60.0, 1.0, 5.0, 0.0);
    let spin = gtk::SpinButton::new(Some(&adj), 1.0, 0);
    grid.attach(&row_poll, 0, 0, 1, 1);
    grid.attach(&spin, 1, 0, 1, 1);

    let row_sec = icon_label_row(
        "view-list-symbolic",
        &format!("<b>{}</b>", i18n_init.t("set.show_seconds")),
    );
    let sw_sec = gtk::Switch::new();
    sw_sec.set_active(s.show_seconds);
    sw_sec.set_halign(gtk::Align::Start);
    grid.attach(&row_sec, 0, 1, 1, 1);
    grid.attach(&sw_sec, 1, 1, 1, 1);

    let row_codec = icon_label_row(
        "audio-card-symbolic",
        &format!("<b>{}</b>", i18n_init.t("set.show_codec")),
    );
    let sw_codec = gtk::Switch::new();
    sw_codec.set_active(s.show_codec);
    sw_codec.set_halign(gtk::Align::Start);
    grid.attach(&row_codec, 0, 2, 1, 1);
    grid.attach(&sw_codec, 1, 2, 1, 1);

    let row_only = icon_label_row(
        "media-playback-start-symbolic",
        &format!("<b>{}</b>", i18n_init.t("set.only_playback")),
    );
    let sw_only = gtk::Switch::new();
    sw_only.set_active(s.only_playback);
    sw_only.set_halign(gtk::Align::Start);
    grid.attach(&row_only, 0, 3, 1, 1);
    grid.attach(&sw_only, 1, 3, 1, 1);

    let row_wired = icon_label_row(
        "audio-headphones-symbolic",
        &format!("<b>{}</b>", i18n_init.t("set.track_wired")),
    );
    let sw_wired = gtk::Switch::new();
    sw_wired.set_active(s.track_wired_usb);
    sw_wired.set_halign(gtk::Align::Start);
    grid.attach(&row_wired, 0, 4, 1, 1);
    grid.attach(&sw_wired, 1, 4, 1, 1);

    let row_sysd = icon_label_row(
        "system-run-symbolic",
        &format!("<b>{}</b>", i18n_init.t("set.systemd")),
    );
    let sw_sysd = gtk::Switch::new();
    sw_sysd.set_active(is_systemd_enabled());
    sw_sysd.set_halign(gtk::Align::Start);
    grid.attach(&row_sysd, 0, 5, 1, 1);
    grid.attach(&sw_sysd, 1, 5, 1, 1);

    let row_lang = icon_label_row(
        "preferences-desktop-locale-symbolic",
        &format!("<b>{}</b>", i18n_init.t("set.language")),
    );
    let cb_lang = gtk::ComboBoxText::new();
    cb_lang.append(Some("es"), "Español (es.pak)");
    cb_lang.append(Some("en"), "English (en.pak)");
    cb_lang.set_active_id(Some(&s.language));
    grid.attach(&row_lang, 0, 6, 1, 1);
    grid.attach(&cb_lang, 1, 6, 1, 1);

    let row_chart = icon_label_row(
        "utilities-system-monitor-symbolic",
        &format!("<b>{}</b>", i18n_init.t("set.chart_type")),
    );
    let cb_chart = gtk::ComboBoxText::new();
    cb_chart.append_text(i18n_init.t("set.chart_lines"));
    cb_chart.append_text(i18n_init.t("set.chart_bars"));
    cb_chart.append_text(i18n_init.t("set.chart_pie"));
    cb_chart.set_active(Some(s.chart_type.to_index()));
    grid.attach(&row_chart, 0, 7, 1, 1);
    grid.attach(&cb_chart, 1, 7, 1, 1);

    let row_theme = icon_label_row(
        "applications-graphics-symbolic",
        &format!("<b>{}</b>", i18n_init.t("set.theme_color")),
    );
    let cb_theme = gtk::ComboBoxText::new();
    cb_theme.append_text(i18n_init.t("set.color_emerald"));
    cb_theme.append_text(i18n_init.t("set.color_cyber"));
    cb_theme.append_text(i18n_init.t("set.color_amber"));
    cb_theme.append_text(i18n_init.t("set.color_crimson"));
    cb_theme.set_active(Some(s.theme_color.to_index()));
    grid.attach(&row_theme, 0, 8, 1, 1);
    grid.attach(&cb_theme, 1, 8, 1, 1);

    let action_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let btn_save = gtk::Button::new();
    btn_save.add(&icon_label_row(
        "document-save-symbolic",
        &format!("<b>{}</b>", i18n_init.t("set.btn_save")),
    ));

    let btn_revert = gtk::Button::new();
    btn_revert.add(&icon_label_row(
        "edit-undo-symbolic",
        i18n_init.t("set.btn_revert"),
    ));

    let btn_default = gtk::Button::new();
    btn_default.add(&icon_label_row(
        "view-refresh-symbolic",
        i18n_init.t("set.btn_default"),
    ));

    action_box.pack_start(&btn_save, true, true, 0);
    action_box.pack_start(&btn_revert, true, true, 0);
    action_box.pack_start(&btn_default, true, true, 0);
    tab_settings.pack_start(&action_box, false, false, 4);

    let lbl_settings_status = gtk::Label::new(None);
    lbl_settings_status.set_markup(&format!(
        "<small><i>{}</i></small>",
        i18n_init.t("set.hint")
    ));
    lbl_settings_status.set_xalign(0.0);
    tab_settings.pack_start(&lbl_settings_status, false, false, 0);

    let sep = gtk::Separator::new(gtk::Orientation::Horizontal);
    tab_settings.pack_start(&sep, false, false, 4);

    let btn_import = gtk::Button::new();
    btn_import.add(&icon_label_row(
        "document-open-symbolic",
        i18n_init.t("set.btn_import"),
    ));
    tab_settings.pack_start(&btn_import, false, false, 2);

    let btn_debug = gtk::Button::new();
    btn_debug.add(&icon_label_row(
        "utilities-terminal-symbolic",
        i18n_init.t("set.btn_debug"),
    ));
    tab_settings.pack_start(&btn_debug, false, false, 2);

    // DIBUJO DE GRÁFICA HISTÓRICA (SOPORTA LÍNEAS RECTAS, BARRAS VERTICALES Y TORTA/PASTEL)
    {
        let tracker = tracker.clone();
        let hist_range = hist_range.clone();
        hist_drawing.connect_draw(move |widget, cr| {
            let w = widget.allocated_width() as f64;
            let h = widget.allocated_height() as f64;
            let pad_l = 54.0;
            let pad_r = 22.0;
            let pad_t = 18.0;
            let pad_b = 30.0;
            let gw = (w - pad_l - pad_r).max(10.0);
            let gh = (h - pad_t - pad_b).max(10.0);

            cr.set_source_rgb(0.11, 0.12, 0.14);
            let _ = cr.paint();

            let t = tracker.borrow();
            let chart_type = t.db.settings.chart_type;
            let (rgb1, rgb2, _, _) = t.db.settings.theme_color.palette_rgb();
            let buckets = compute_history_buckets(&t.db, *hist_range.borrow());

            if chart_type == ChartType::Pie {
                let tot_conn: u64 = buckets.iter().map(|b| b.connected_sec).sum();
                let tot_play: u64 = buckets.iter().map(|b| b.listening_sec).sum();
                let idle_sec = tot_conn.saturating_sub(tot_play);
                let total = (tot_play + idle_sec).max(1) as f64;

                let cx = w * 0.35;
                let cy = h * 0.50;
                let radius = (h.min(w) * 0.36).max(20.0);

                let play_angle = (tot_play as f64 / total) * std::f64::consts::TAU;

                // Sector Reproduciendo
                cr.set_source_rgb(rgb2.0, rgb2.1, rgb2.2);
                cr.move_to(cx, cy);
                cr.arc(cx, cy, radius, -std::f64::consts::FRAC_PI_2, -std::f64::consts::FRAC_PI_2 + play_angle);
                cr.close_path();
                let _ = cr.fill();

                // Sector Conectado en reposo
                cr.set_source_rgb(rgb1.0, rgb1.1, rgb1.2);
                cr.move_to(cx, cy);
                cr.arc(cx, cy, radius, -std::f64::consts::FRAC_PI_2 + play_angle, -std::f64::consts::FRAC_PI_2 + std::f64::consts::TAU);
                cr.close_path();
                let _ = cr.fill();

                // Leyenda lateral
                let lx = w * 0.60;
                cr.set_source_rgb(rgb1.0, rgb1.1, rgb1.2);
                cr.rectangle(lx, cy - 24.0, 14.0, 14.0);
                let _ = cr.fill();
                cr.set_source_rgba(0.92, 0.92, 0.92, 0.95);
                cr.move_to(lx + 22.0, cy - 13.0);
                let _ = cr.show_text(&format!(
                    "{}: {}",
                    t.i18n.t("hist.connected"),
                    format_short(tot_conn)
                ));

                cr.set_source_rgb(rgb2.0, rgb2.1, rgb2.2);
                cr.rectangle(lx, cy + 6.0, 14.0, 14.0);
                let _ = cr.fill();
                cr.set_source_rgba(0.92, 0.92, 0.92, 0.95);
                cr.move_to(lx + 22.0, cy + 17.0);
                let pct = if tot_conn > 0 {
                    (tot_play as f64 / tot_conn as f64) * 100.0
                } else {
                    0.0
                };
                let _ = cr.show_text(&format!(
                    "{}: {} ({:.0}%)",
                    t.i18n.t("hist.playing"),
                    format_short(tot_play),
                    pct
                ));

                return glib::Propagation::Proceed;
            }

            let max_sec = buckets
                .iter()
                .map(|b| b.connected_sec.max(b.listening_sec))
                .max()
                .unwrap_or(3600)
                .max(60) as f64;

            cr.set_line_width(1.0);
            for step in 0..=4 {
                let frac = step as f64 / 4.0;
                let y = pad_t + gh * (1.0 - frac);
                cr.set_source_rgba(1.0, 1.0, 1.0, 0.12);
                cr.move_to(pad_l, y);
                cr.line_to(pad_l + gw, y);
                let _ = cr.stroke();

                let val_sec = (max_sec * frac) as u64;
                let lbl = if val_sec >= 3600 {
                    format!("{:.1}h", val_sec as f64 / 3600.0)
                } else {
                    format!("{}m", val_sec / 60)
                };
                cr.set_source_rgba(0.85, 0.85, 0.85, 0.85);
                cr.move_to(6.0, y + 4.0);
                let _ = cr.show_text(&lbl);
            }

            let n = buckets.len();
            if n > 0 {
                if chart_type == ChartType::Bars {
                    let group_w = gw / n as f64;
                    let bar_w = (group_w * 0.32).clamp(4.0, 26.0);
                    for (i, b) in buckets.iter().enumerate() {
                        let cx = pad_l + (i as f64 + 0.5) * group_w;
                        let h_conn = gh * (b.connected_sec as f64 / max_sec).clamp(0.0, 1.0);
                        let h_play = gh * (b.listening_sec as f64 / max_sec).clamp(0.0, 1.0);

                        cr.set_source_rgb(rgb1.0, rgb1.1, rgb1.2);
                        cr.rectangle(cx - bar_w - 1.5, pad_t + gh - h_conn, bar_w, h_conn);
                        let _ = cr.fill();

                        cr.set_source_rgb(rgb2.0, rgb2.1, rgb2.2);
                        cr.rectangle(cx + 1.5, pad_t + gh - h_play, bar_w, h_play);
                        let _ = cr.fill();

                        cr.set_source_rgba(0.85, 0.85, 0.85, 0.90);
                        cr.move_to((cx - 14.0).max(pad_l - 10.0), h - 8.0);
                        let _ = cr.show_text(&b.label);
                    }
                } else {
                    let step_x = if n > 1 { gw / (n - 1) as f64 } else { gw };

                    cr.set_source_rgb(rgb1.0, rgb1.1, rgb1.2);
                    cr.set_line_width(2.5);
                    for (i, b) in buckets.iter().enumerate() {
                        let x = pad_l + i as f64 * step_x;
                        let y = pad_t + gh * (1.0 - (b.connected_sec as f64 / max_sec).clamp(0.0, 1.0));
                        if i == 0 {
                            cr.move_to(x, y);
                        } else {
                            cr.line_to(x, y);
                        }
                    }
                    let _ = cr.stroke();

                    for (i, b) in buckets.iter().enumerate() {
                        let x = pad_l + i as f64 * step_x;
                        let y = pad_t + gh * (1.0 - (b.connected_sec as f64 / max_sec).clamp(0.0, 1.0));
                        cr.arc(x, y, 3.5, 0.0, std::f64::consts::TAU);
                        let _ = cr.fill();
                    }

                    cr.set_source_rgb(rgb2.0, rgb2.1, rgb2.2);
                    cr.set_line_width(2.2);
                    for (i, b) in buckets.iter().enumerate() {
                        let x = pad_l + i as f64 * step_x;
                        let y = pad_t + gh * (1.0 - (b.listening_sec as f64 / max_sec).clamp(0.0, 1.0));
                        if i == 0 {
                            cr.move_to(x, y);
                        } else {
                            cr.line_to(x, y);
                        }
                    }
                    let _ = cr.stroke();

                    for (i, b) in buckets.iter().enumerate() {
                        let x = pad_l + i as f64 * step_x;
                        let y = pad_t + gh * (1.0 - (b.listening_sec as f64 / max_sec).clamp(0.0, 1.0));
                        cr.arc(x, y, 3.0, 0.0, std::f64::consts::TAU);
                        let _ = cr.fill();

                        cr.set_source_rgba(0.85, 0.85, 0.85, 0.90);
                        cr.move_to((x - 14.0).max(pad_l - 10.0), h - 8.0);
                        let _ = cr.show_text(&b.label);
                    }
                }
            }

            glib::Propagation::Proceed
        });
    }

    let refresh_ui: Rc<dyn Fn()> = {
        let live_box = live_box.clone();
        let hist_devices_box = hist_devices_box.clone();
        let hist_drawing = hist_drawing.clone();
        let lbl_hist_summary = lbl_hist_summary.clone();
        let tracker = tracker.clone();
        let hist_range = hist_range.clone();
        let expanded_devs = expanded_devs.clone();
        let win_weak = win.downgrade();

        Rc::new(move || {
            for child in live_box.children() {
                live_box.remove(&child);
            }
            for child in hist_devices_box.children() {
                hist_devices_box.remove(&child);
            }

            let t = tracker.borrow();
            let s = &t.db.settings;
            let i18n = &t.i18n;
            let (_, _, hex1, hex2) = s.theme_color.palette_rgb();

            for (mac, dev) in &t.db.devices {
                let status_icon = if dev.conn_type == "Bluetooth" {
                    if dev.currently_connected {
                        "network-bluetooth-activated-symbolic"
                    } else {
                        "network-bluetooth-symbolic"
                    }
                } else {
                    "audio-headphones-symbolic"
                };

                let status_txt = if dev.currently_connected {
                    format!(
                        "<span foreground='{}'><b>{}</b></span>",
                        hex1,
                        i18n.t("status.connected")
                    )
                } else {
                    format!(
                        "<span foreground='#9ca3af'><b>{}</b></span>",
                        i18n.t("status.disconnected")
                    )
                };
                let codec = if s.show_codec && dev.currently_connected {
                    format!(
                        "  |  <b>{}</b>",
                        dev.current_codec.as_deref().unwrap_or("PCM/SBC")
                    )
                } else {
                    String::new()
                };

                let real_tag = if !dev.real_name.is_empty() && dev.real_name != dev.name {
                    format!(" <i>[{}]</i>", glib::markup_escape_text(&dev.real_name))
                } else {
                    String::new()
                };

                let frame = gtk::Frame::new(None);
                let hdr_box = icon_label_row(
                    status_icon,
                    &format!(
                        "<b>{}</b>{} <small>({}, {})</small>  —  {}{}",
                        glib::markup_escape_text(&dev.name),
                        real_tag,
                        glib::markup_escape_text(&dev.conn_type),
                        glib::markup_escape_text(mac),
                        status_txt,
                        codec
                    ),
                );
                frame.set_label_widget(Some(&hdr_box));

                let card = gtk::Box::new(gtk::Orientation::Vertical, 8);
                card.set_border_width(12);
                frame.add(&card);

                let active_sess = dev.sessions.last().filter(|x| x.status == "active");
                let cur_conn_sec = active_sess.map(|x| x.connected_seconds).unwrap_or(0);
                let cur_play_sec = active_sess.map(|x| x.listening_seconds).unwrap_or(0);

                if s.only_playback == false {
                    let conn_str = if active_sess.is_some() {
                        format_time(cur_conn_sec, s.show_seconds)
                    } else {
                        format!(
                            "0m 00s <small><i>({})</i></small>",
                            i18n.t("dev.no_active_session")
                        )
                    };
                    card.pack_start(
                        &icon_label_row(
                            "preferences-system-time-symbolic",
                            &format!("<b>{}:</b>  {}", i18n.t("dev.conn_session"), conn_str),
                        ),
                        false,
                        false,
                        0,
                    );
                }

                let play_str = if active_sess.is_some() {
                    format_time(cur_play_sec, s.show_seconds)
                } else {
                    "0m 00s".into()
                };
                card.pack_start(
                    &icon_label_row(
                        "media-playback-start-symbolic",
                        &format!("<b>{}:</b>  {}", i18n.t("dev.play_session"), play_str),
                    ),
                    false,
                    false,
                    0,
                );

                if let Some(bat) = dev.current_battery.or_else(|| {
                    dev.sessions
                        .iter()
                        .rev()
                        .find_map(|x| x.battery_end.or(x.battery_start))
                }) {
                    let b_box = gtk::Box::new(gtk::Orientation::Horizontal, 10);
                    let bat_icon = if bat >= 80 {
                        "battery-full-symbolic"
                    } else if bat >= 40 {
                        "battery-good-symbolic"
                    } else {
                        "battery-low-symbolic"
                    };
                    let blbl = icon_label_row(
                        bat_icon,
                        &format!("<b>{} ({}%):</b>", i18n.t("dev.battery"), bat),
                    );
                    let bar = gtk::ProgressBar::new();
                    bar.set_fraction((bat as f64 / 100.0).clamp(0.0, 1.0));
                    b_box.pack_start(&blbl, false, false, 0);
                    b_box.pack_start(&bar, true, true, 0);
                    card.pack_start(&b_box, false, false, 4);
                } else {
                    let (no_bat_icon, no_bat_txt) = if dev.conn_type == "Bluetooth" {
                        (
                            "battery-missing-symbolic",
                            format!("<b>{}:</b>  {}", i18n.t("dev.battery"), i18n.t("dev.bt_no_bat")),
                        )
                    } else {
                        (
                            "ac-adapter-symbolic",
                            format!("<b>{}:</b>  {}", i18n.t("dev.power"), i18n.t("dev.wired_no_bat")),
                        )
                    };
                    card.pack_start(&icon_label_row(no_bat_icon, &no_bat_txt), false, false, 2);
                }

                if let Some(act_sess) = active_sess {
                    let live_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
                    let v_now = dev.current_volume.or(act_sess.volume_avg).unwrap_or(0);
                    let inc_badge = if act_sess.volume_increased {
                        " <span foreground='#facc15'><b>↑</b></span>"
                    } else {
                        ""
                    };
                    let lbl_live = icon_label_row(
                        "multimedia-volume-control-symbolic",
                        &format!(
                            "<small>{}: {}  |  {}: <b>{}%</b>{}</small>",
                            i18n.t("dev.session_started"),
                            act_sess.start.replace('T', " "),
                            i18n.t("dev.current_vol"),
                            v_now,
                            inc_badge
                        ),
                    );
                    let btn_live_graph = gtk::Button::new();
                    btn_live_graph.add(&icon_label_row(
                        "utilities-system-monitor-symbolic",
                        i18n.t("dev.live_graph"),
                    ));

                    let mac_cl = mac.clone();
                    let dev_cl = dev.clone();
                    let sess_cl = act_sess.clone();
                    let i18n_cl = i18n.clone();
                    let theme_cl = s.theme_color;
                    let win_wk = win_weak.clone();
                    btn_live_graph.connect_clicked(move |_| {
                        if let Some(w) = win_wk.upgrade() {
                            show_session_details_dialog(
                                &w, &mac_cl, &dev_cl, &sess_cl, &i18n_cl, theme_cl,
                            );
                        }
                    });

                    live_row.pack_start(&lbl_live, true, true, 0);
                    live_row.pack_end(&btn_live_graph, false, false, 0);
                    card.pack_start(&live_row, false, false, 2);
                }

                live_box.pack_start(&frame, false, false, 0);
            }

            let buckets = compute_history_buckets(&t.db, *hist_range.borrow());
            let tot_conn: u64 = buckets.iter().map(|b| b.connected_sec).sum();
            let tot_play: u64 = buckets.iter().map(|b| b.listening_sec).sum();
            let avg_conn = tot_conn / (buckets.len().max(1) as u64);
            let avg_play = tot_play / (buckets.len().max(1) as u64);

            lbl_hist_summary.set_markup(&format!(
                "<b>{}:</b> {}: <span foreground='{}'><b>{}</b></span> ({}: {})   |   {}: <span foreground='{}'><b>{}</b></span> ({}: {})",
                i18n.t("hist.summary_prefix"),
                i18n.t("hist.connected"),
                hex1,
                format_time(tot_conn, s.show_seconds),
                i18n.t("hist.avg"),
                format_short(avg_conn),
                i18n.t("hist.playing"),
                hex2,
                format_time(tot_play, s.show_seconds),
                i18n.t("hist.avg"),
                format_short(avg_play)
            ));
            hist_drawing.queue_draw();

            for (mac, dev) in &t.db.devices {
                let real_tag = if !dev.real_name.is_empty() && dev.real_name != dev.name {
                    format!(" <i>[{}]</i>", glib::markup_escape_text(&dev.real_name))
                } else {
                    String::new()
                };

                let frame = gtk::Frame::new(None);
                let hdr_box = icon_label_row(
                    "audio-headphones-symbolic",
                    &format!(
                        "<b>{}</b>{} <small>({}, {})</small>  —  <b>{}</b>",
                        glib::markup_escape_text(&dev.name),
                        real_tag,
                        glib::markup_escape_text(&dev.conn_type),
                        glib::markup_escape_text(mac),
                        i18n.t("hist.total_header")
                    ),
                );
                frame.set_label_widget(Some(&hdr_box));

                let card = gtk::Box::new(gtk::Orientation::Vertical, 8);
                card.set_border_width(12);
                frame.add(&card);

                if s.only_playback == false {
                    card.pack_start(
                        &icon_label_row(
                            "preferences-system-time-symbolic",
                            &format!(
                                "<b>{}:</b>  {}",
                                i18n.t("hist.total_connected"),
                                format_time(dev.lifetime_connected_seconds, s.show_seconds)
                            ),
                        ),
                        false,
                        false,
                        0,
                    );
                }

                card.pack_start(
                    &icon_label_row(
                        "media-playback-start-symbolic",
                        &format!(
                            "<b>{}:</b>  {}",
                            i18n.t("hist.total_playing"),
                            format_time(dev.lifetime_listening_seconds, s.show_seconds)
                        ),
                    ),
                    false,
                    false,
                    0,
                );

                let expander = gtk::Expander::new(None);
                let exp_lbl = icon_label_row(
                    "view-list-symbolic",
                    &format!("<b>{} ({})</b>", i18n.t("hist.sessions_expander"), dev.sessions.len()),
                );
                expander.set_label_widget(Some(&exp_lbl));
                expander.set_expanded(expanded_devs.borrow().contains(mac));

                {
                    let mac_cl = mac.clone();
                    let exp_set = expanded_devs.clone();
                    expander.connect_activate(move |exp| {
                        let will_expand = exp.is_expanded() == false;
                        if will_expand {
                            exp_set.borrow_mut().insert(mac_cl.clone());
                        } else {
                            exp_set.borrow_mut().remove(&mac_cl);
                        }
                    });
                }

                let sess_frame = gtk::Frame::new(None);
                let sess_box = gtk::Box::new(gtk::Orientation::Vertical, 6);
                sess_box.set_border_width(10);
                sess_frame.add(&sess_box);

                for sess in dev.sessions.iter().rev().take(15) {
                    let b_txt = match (sess.battery_start, sess.battery_end) {
                        (Some(b1), Some(b2)) => format!("  |  Bat: {}% ➔ {}%", b1, b2),
                        _ => String::new(),
                    };
                    let v_txt = sess
                        .volume_avg
                        .map(|v| {
                            format!(
                                "  |  Vol: {}%{}",
                                v,
                                if sess.volume_increased { " ↑" } else { "" }
                            )
                        })
                        .unwrap_or_default();

                    let s_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
                    let info_lbl = icon_label_row(
                        "x-office-calendar-symbolic",
                        &format!(
                            "{}  |  {}: {}  |  {}: {}{}{}  [{}]",
                            &sess.start.replace('T', " "),
                            i18n.t("hist.connected"),
                            sess.connected_human,
                            i18n.t("hist.playing"),
                            sess.listening_human,
                            b_txt,
                            v_txt,
                            sess.status
                        ),
                    );

                    let btn_details = gtk::Button::from_icon_name(
                        Some("utilities-system-monitor-symbolic"),
                        gtk::IconSize::Button,
                    );

                    let mac_cl = mac.clone();
                    let dev_cl = dev.clone();
                    let sess_cl = sess.clone();
                    let i18n_cl = i18n.clone();
                    let theme_cl = s.theme_color;
                    let win_wk = win_weak.clone();
                    btn_details.connect_clicked(move |_| {
                        if let Some(w) = win_wk.upgrade() {
                            show_session_details_dialog(
                                &w, &mac_cl, &dev_cl, &sess_cl, &i18n_cl, theme_cl,
                            );
                        }
                    });

                    s_row.pack_start(&info_lbl, true, true, 0);
                    s_row.pack_end(&btn_details, false, false, 0);
                    sess_box.pack_start(&s_row, false, false, 2);
                }

                expander.add(&sess_frame);
                card.pack_start(&expander, false, false, 4);

                hist_devices_box.pack_start(&frame, false, false, 0);
            }

            live_box.show_all();
            hist_devices_box.show_all();
        })
    };

    {
        let hr = hist_range.clone();
        let refresh = refresh_ui.clone();
        btn_7d.connect_clicked(move |_| {
            *hr.borrow_mut() = HistoryRange::Days7;
            refresh();
        });
    }
    {
        let hr = hist_range.clone();
        let refresh = refresh_ui.clone();
        btn_12m.connect_clicked(move |_| {
            *hr.borrow_mut() = HistoryRange::Months12;
            refresh();
        });
    }
    {
        let hr = hist_range.clone();
        let refresh = refresh_ui.clone();
        btn_yr.connect_clicked(move |_| {
            *hr.borrow_mut() = HistoryRange::Years;
            refresh();
        });
    }

    {
        let tracker = tracker.clone();
        let refresh = refresh_ui.clone();
        let spin = spin.clone();
        let sw_sec = sw_sec.clone();
        let sw_codec = sw_codec.clone();
        let sw_only = sw_only.clone();
        let sw_wired = sw_wired.clone();
        let sw_sysd = sw_sysd.clone();
        let cb_lang = cb_lang.clone();
        let cb_chart = cb_chart.clone();
        let cb_theme = cb_theme.clone();
        let lbl_status = lbl_settings_status.clone();

        btn_save.connect_clicked(move |_| {
            let msg = {
                let mut t = tracker.borrow_mut();
                t.db.settings.poll_interval = spin.value_as_int() as u64;
                t.db.settings.show_seconds = sw_sec.is_active();
                t.db.settings.show_codec = sw_codec.is_active();
                t.db.settings.only_playback = sw_only.is_active();
                t.db.settings.track_wired_usb = sw_wired.is_active();
                if let Some(lang_id) = cb_lang.active_id() {
                    t.db.settings.language = lang_id.to_string();
                }
                t.db.settings.chart_type = ChartType::from_index(cb_chart.active().unwrap_or(0));
                t.db.settings.theme_color = ThemeColor::from_index(cb_theme.active().unwrap_or(0));
                t.reload_i18n();
                t.save_db();
                t.tick(false);
                t.i18n.t("set.saved_ok").to_string()
            };
            set_systemd_service(sw_sysd.is_active());
            refresh();
            lbl_status.set_markup(&format!(
                "<span foreground='#4ade80'><b>{}</b></span>",
                msg
            ));
        });
    }

    {
        let tracker = tracker.clone();
        let spin = spin.clone();
        let sw_sec = sw_sec.clone();
        let sw_codec = sw_codec.clone();
        let sw_only = sw_only.clone();
        let sw_wired = sw_wired.clone();
        let sw_sysd = sw_sysd.clone();
        let cb_lang = cb_lang.clone();
        let cb_chart = cb_chart.clone();
        let cb_theme = cb_theme.clone();
        let lbl_status = lbl_settings_status.clone();

        btn_revert.connect_clicked(move |_| {
            let t = tracker.borrow();
            let s = &t.db.settings;
            spin.set_value(s.poll_interval as f64);
            sw_sec.set_active(s.show_seconds);
            sw_codec.set_active(s.show_codec);
            sw_only.set_active(s.only_playback);
            sw_wired.set_active(s.track_wired_usb);
            sw_sysd.set_active(is_systemd_enabled());
            cb_lang.set_active_id(Some(&s.language));
            cb_chart.set_active(Some(s.chart_type.to_index()));
            cb_theme.set_active(Some(s.theme_color.to_index()));
            lbl_status.set_markup(&format!(
                "<span foreground='#60a5fa'><b>{}</b></span>",
                t.i18n.t("set.reverted_ok")
            ));
        });
    }

    {
        let tracker = tracker.clone();
        let refresh = refresh_ui.clone();
        let spin = spin.clone();
        let sw_sec = sw_sec.clone();
        let sw_codec = sw_codec.clone();
        let sw_only = sw_only.clone();
        let sw_wired = sw_wired.clone();
        let cb_lang = cb_lang.clone();
        let cb_chart = cb_chart.clone();
        let cb_theme = cb_theme.clone();
        let lbl_status = lbl_settings_status.clone();

        btn_default.connect_clicked(move |_| {
            let def = Settings::default();
            spin.set_value(def.poll_interval as f64);
            sw_sec.set_active(def.show_seconds);
            sw_codec.set_active(def.show_codec);
            sw_only.set_active(def.only_playback);
            sw_wired.set_active(def.track_wired_usb);
            cb_lang.set_active_id(Some(&def.language));
            cb_chart.set_active(Some(def.chart_type.to_index()));
            cb_theme.set_active(Some(def.theme_color.to_index()));
            let msg = {
                let mut t = tracker.borrow_mut();
                t.db.settings = def;
                t.reload_i18n();
                t.save_db();
                t.tick(false);
                t.i18n.t("set.default_ok").to_string()
            };
            refresh();
            lbl_status.set_markup(&format!(
                "<span foreground='#facc15'><b>{}</b></span>",
                msg
            ));
        });
    }

    {
        let win_weak = win.downgrade();
        let tracker = tracker.clone();
        let refresh = refresh_ui.clone();
        btn_import.connect_clicked(move |_| {
            let Some(win) = win_weak.upgrade() else { return };
            let dlg = gtk::FileChooserDialog::new(
                Some("Import JSON"),
                Some(&win),
                gtk::FileChooserAction::Open,
            );
            dlg.add_button("Cancel", gtk::ResponseType::Cancel);
            dlg.add_button("Open", gtk::ResponseType::Accept);

            if dlg.run() == gtk::ResponseType::Accept {
                if let Some(path) = dlg.filename() {
                    if let Some(p_str) = path.to_str() {
                        let _ = tracker.borrow_mut().import_external_db(p_str);
                        refresh();
                    }
                }
            }
            unsafe {
                dlg.destroy();
            }
        });
    }

    {
        let win_weak = win.downgrade();
        btn_debug.connect_clicked(move |_| {
            let Some(win) = win_weak.upgrade() else { return };
            let dlg = gtk::FileChooserDialog::new(
                Some("Save Debug Log"),
                Some(&win),
                gtk::FileChooserAction::Save,
            );
            dlg.add_button("Cancel", gtk::ResponseType::Cancel);
            dlg.add_button("Save", gtk::ResponseType::Accept);
            dlg.set_current_name(&format!(
                "ylih_debug_{}.log",
                Local::now().format("%Y%m%d_%H%M%S")
            ));

            if dlg.run() == gtk::ResponseType::Accept {
                if let Some(path) = dlg.filename() {
                    let _ = write_debug_log(&path);
                }
            }
            unsafe {
                dlg.destroy();
            }
        });
    }

    refresh_ui();

    {
        let tracker = tracker.clone();
        let refresh = refresh_ui.clone();
        glib::timeout_add_local_once(Duration::from_millis(150), move || {
            tracker.borrow_mut().clean_orphan_sessions();
            tracker.borrow_mut().tick(true);
            refresh();
        });
    }

    {
        let tracker = tracker.clone();
        let refresh = refresh_ui.clone();
        glib::timeout_add_seconds_local(2, move || {
            tracker.borrow_mut().tick(false);
            refresh();
            glib::ControlFlow::Continue
        });
    }

    win.connect_destroy(|_| gtk::main_quit());
    win.show_all();
    gtk::main();
}
