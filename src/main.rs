// SPDX-License-Identifier: GPL-3.0-or-later
use chrono::Local;
use crossterm::{
    event::{self, Event, KeyCode},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use gtk::prelude::*;
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph},
    Terminal,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    env, fs,
    io::stdout,
    path::PathBuf,
    process::Command,
    rc::Rc,
    cell::RefCell,
    time::{Duration, Instant},
};
use zbus::{blocking::Connection, zvariant::OwnedValue};

const AUDIO_UUIDS: &[&str] = &[
    "0000110b-0000-1000-8000-00805f9b34fb",
    "00001108-0000-1000-8000-00805f9b34fb",
    "0000111e-0000-1000-8000-00805f9b34fb",
    "0000110d-0000-1000-8000-00805f9b34fb",
];

#[derive(Serialize, Deserialize, Clone, Debug)]
struct Settings {
    #[serde(default = "default_poll")]
    poll_interval: u64,
    #[serde(default = "default_true")]
    show_seconds: bool,
    #[serde(default)]
    only_playback: bool,
    #[serde(default = "default_true")]
    show_codec: bool,
}

fn default_poll() -> u64 { 5 }
fn default_true() -> bool { true }

impl Default for Settings {
    fn default() -> Self {
        Self {
            poll_interval: 5,
            show_seconds: true,
            only_playback: false,
            show_codec: true,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
struct Session {
    start: String,
    end: String,
    connected_seconds: u64,
    listening_seconds: u64,
    connected_human: String,
    listening_human: String,
    battery_start: Option<u8>,
    battery_end: Option<u8>,
    #[serde(default)]
    codec: Option<String>,
    status: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
struct DeviceRecord {
    name: String,
    lifetime_connected_seconds: u64,
    lifetime_listening_seconds: u64,
    lifetime_connected_human: String,
    lifetime_listening_human: String,
    sessions: Vec<Session>,
    #[serde(default)]
    currently_connected: bool,
    #[serde(default)]
    current_battery: Option<u8>,
    #[serde(default)]
    current_codec: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
struct Database {
    #[serde(default)]
    settings: Settings,
    #[serde(default)]
    devices: BTreeMap<String, DeviceRecord>,
}

#[derive(Clone, Debug)]
struct BluezInfo {
    name: String,
    connected: bool,
    battery: Option<u8>,
    codec: Option<String>,
}

fn log_path() -> PathBuf {
    let home = env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".local/share/ylih/ylih_log.json")
}

fn service_path() -> PathBuf {
    let home = env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".config/systemd/user/ylih.service")
}

fn format_short(sec: u64) -> String {
    let h = sec / 3600;
    let m = (sec % 3600) / 60;
    let s = sec % 60;
    if h > 0 {
        format!("{}h {:02}m", h, m)
    } else {
        format!("{}m {:02}s", m, s)
    }
}

fn format_time(sec: u64, show_sec: bool) -> String {
    let h = sec / 3600;
    let m = (sec % 3600) / 60;
    let s = sec % 60;
    if show_sec {
        if h > 0 {
            format!("{}h {:02}m {:02}s ({}s)", h, m, s, sec)
        } else {
            format!("{}m {:02}s ({}s)", m, s, sec)
        }
    } else if h > 0 {
        format!("{}h {:02}m", h, m)
    } else {
        format!("{}m", m)
    }
}

fn get_pipewire_codecs() -> HashMap<String, String> {
    let mut map = HashMap::new();
    if let Ok(out) = Command::new("pw-dump").output() {
        if let Ok(json) = serde_json::from_slice::<serde_json::Value>(&out.stdout) {
            if let Some(arr) = json.as_array() {
                for obj in arr {
                    if let Some(props) = obj.pointer("/info/props") {
                        let mac = props.get("api.bluez5.address")
                            .or_else(|| props.get("device.string"))
                            .and_then(|v| v.as_str());
                        let codec = props.get("api.bluez5.codec").and_then(|v| v.as_str());
                        if let (Some(m), Some(c)) = (mac, codec) {
                            map.insert(m.to_uppercase(), c.to_uppercase());
                        }
                    }
                }
            }
        }
    }
    map
}

fn query_bluez_devices() -> BTreeMap<String, BluezInfo> {
    let mut result = BTreeMap::new();
    let pw_codecs = get_pipewire_codecs();

    let Ok(conn) = Connection::system() else { return result };
    let Ok(reply) = conn.call_method(
        Some("org.bluez"),
        "/",
        Some("org.freedesktop.DBus.ObjectManager"),
        "GetManagedObjects",
        &(),
    ) else { return result };

    type ManagedObjects = HashMap<
        zbus::zvariant::OwnedObjectPath,
        HashMap<String, HashMap<String, OwnedValue>>,
    >;

    let Ok(objects) = reply.body().deserialize::<ManagedObjects>() else {
        return result;
    };

    for (_path, ifaces) in objects {
        let Some(dev) = ifaces.get("org.bluez.Device1") else { continue };

        let icon = dev.get("Icon").and_then(|v| <&str>::try_from(v).ok()).unwrap_or("");
        let mut is_audio = icon.contains("audio") || icon.contains("headset") || icon.contains("headphone");

        if let Some(uuids_val) = dev.get("UUIDs") {
         let uuids_str = format!("{:?}", uuids_val).to_lowercase();
         if AUDIO_UUIDS.iter().any(|u| uuids_str.contains(u)) {
             is_audio = true;
         }
     }

        if !is_audio {
            continue;
        }

        let mac = dev.get("Address")
            .and_then(|v| <&str>::try_from(v).ok())
            .unwrap_or("00:00:00:00:00:00")
            .to_uppercase();

        let name = dev.get("Alias")
            .or_else(|| dev.get("Name"))
            .and_then(|v| <&str>::try_from(v).ok())
            .unwrap_or(&mac)
            .to_string();

        let connected = dev.get("Connected")
            .and_then(|v| bool::try_from(v).ok())
            .unwrap_or(false);

        let battery = ifaces.get("org.bluez.Battery1")
            .and_then(|b| b.get("Percentage"))
            .and_then(|v| u8::try_from(v).ok());

        let codec = if connected {
            Some(pw_codecs.get(&mac).cloned().unwrap_or_else(|| "AAC/SBC".to_string()))
        } else {
            None
        };

        result.insert(mac, BluezInfo { name, connected, battery, codec });
    }
    result
}

fn is_audio_playing() -> bool {
    Command::new("playerctl")
        .args(["-a", "status"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).lines().any(|l| l.trim() == "Playing"))
        .unwrap_or(false)
}

fn is_systemd_enabled() -> bool {
    Command::new("systemctl")
        .args(["--user", "is-active", "ylih.service"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "active")
        .unwrap_or(false)
}

fn set_systemd_service(enable: bool) {
    let exe = env::current_exe().unwrap_or_else(|_| PathBuf::from("ylih"));
    let srv_path = service_path();
    if enable {
        if let Some(parent) = srv_path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let unit = format!(
            "[Unit]\nDescription=YLIH Rust - Bluetooth Headphone Lifespan Daemon\nAfter=bluetooth.target pipewire.service\n\n[Service]\nExecStart=\"{}\" --daemon\nRestart=on-failure\nRestartSec=5\n\n[Install]\nWantedBy=default.target\n",
            exe.display()
        );
        let _ = fs::write(&srv_path, unit);
        let _ = Command::new("systemctl").args(["--user", "daemon-reload"]).status();
        let _ = Command::new("systemctl").args(["--user", "enable", "--now", "ylih.service"]).status();
    } else {
        let _ = Command::new("systemctl").args(["--user", "disable", "--now", "ylih.service"]).status();
    }
}

struct Tracker {
    db: Database,
    last_tick: Instant,
}

impl Tracker {
    fn new() -> Self {
        let mut t = Self {
            db: Self::load_db(),
            last_tick: Instant::now(),
        };
        t.clean_orphan_sessions();
        t.tick(true);
        t
    }

    fn load_db() -> Database {
        let path = log_path();
        if let Some(p) = path.parent() {
            let _ = fs::create_dir_all(p);
        }
        if let Ok(content) = fs::read_to_string(&path) {
            if let Ok(db) = serde_json::from_str::<Database>(&content) {
                return db;
            }
        }
        Database::default()
    }

    fn save_db(&self) {
        let path = log_path();
        let tmp = path.with_extension("json.tmp");
        if let Ok(json) = serde_json::to_string_pretty(&self.db) {
            if fs::write(&tmp, json).is_ok() {
                let _ = fs::rename(&tmp, &path);
            }
        }
    }

    fn clean_orphan_sessions(&mut self) {
        let bluez = query_bluez_devices();
        for (mac, dev) in self.db.devices.iter_mut() {
            let is_conn = bluez.get(mac).map(|b| b.connected).unwrap_or(false);
            for s in dev.sessions.iter_mut() {
                if s.status == "active" && !is_conn {
                    s.status = "completed".into();
                }
            }
        }
        self.save_db();
    }

    fn update_totals(dev: &mut DeviceRecord) {
        let total_conn: u64 = dev.sessions.iter().map(|s| s.connected_seconds).sum();
        let total_play: u64 = dev.sessions.iter().map(|s| s.listening_seconds).sum();
        dev.lifetime_connected_seconds = total_conn;
        dev.lifetime_listening_seconds = total_play;
        dev.lifetime_connected_human = format_short(total_conn);
        dev.lifetime_listening_human = format_short(total_play);
    }

    fn import_external_db(&mut self, ext_path: &str) -> Result<usize, String> {
        let content = fs::read_to_string(ext_path).map_err(|e| e.to_string())?;
        let ext: Database = serde_json::from_str(&content).map_err(|e| e.to_string())?;
        let mut imported = 0;

        for (mac, edev) in ext.devices {
            let mac_up = mac.to_uppercase();
            if let Some(local_dev) = self.db.devices.get_mut(&mac_up) {
                let existing: HashSet<String> = local_dev.sessions.iter().map(|s| s.start.clone()).collect();
                for s in edev.sessions {
                    if !existing.contains(&s.start) {
                        local_dev.sessions.push(s);
                        imported += 1;
                    }
                }
                local_dev.sessions.sort_by(|a, b| a.start.cmp(&b.start));
                Self::update_totals(local_dev);
            } else {
                imported += edev.sessions.len();
                self.db.devices.insert(mac_up, edev);
            }
        }
        self.save_db();
        Ok(imported)
    }

    fn tick(&mut self, initial: bool) {
        let elapsed = if initial { 0 } else { self.last_tick.elapsed().as_secs() };
        self.last_tick = Instant::now();
        let now_iso = Local::now().format("%Y-%m-%dT%H:%M:%S").to_string();

        let bluez = query_bluez_devices();
        let playing = is_audio_playing();
        let only_play = self.db.settings.only_playback;

        for (mac, dev) in self.db.devices.iter_mut() {
            if !bluez.contains_key(mac) {
                dev.currently_connected = false;
                dev.current_codec = None;
                if let Some(last) = dev.sessions.last_mut() {
                    if last.status == "active" {
                        last.status = "completed".into();
                        last.end = now_iso.clone();
                    }
                }
            }
        }

        for (mac, binfo) in bluez {
            let dev = self.db.devices.entry(mac).or_insert_with(|| DeviceRecord {
                name: binfo.name.clone(),
                lifetime_connected_seconds: 0,
                lifetime_listening_seconds: 0,
                lifetime_connected_human: "0m 00s".into(),
                lifetime_listening_human: "0m 00s".into(),
                sessions: Vec::new(),
                currently_connected: binfo.connected,
                current_battery: binfo.battery,
                current_codec: binfo.codec.clone(),
            });

            dev.name = binfo.name;
            dev.currently_connected = binfo.connected;
            dev.current_battery = binfo.battery;
            dev.current_codec = binfo.codec.clone();

            if binfo.connected {
                let is_active = dev.sessions.last().map(|s| s.status == "active").unwrap_or(false);
                if !is_active {
                    dev.sessions.push(Session {
                        start: now_iso.clone(),
                        end: now_iso.clone(),
                        connected_seconds: 0,
                        listening_seconds: 0,
                        connected_human: "0m 00s".into(),
                        listening_human: "0m 00s".into(),
                        battery_start: binfo.battery,
                        battery_end: binfo.battery,
                        codec: binfo.codec,
                        status: "active".into(),
                    });
                } else if let Some(active) = dev.sessions.last_mut() {
                    active.end = now_iso.clone();
                    if binfo.codec.is_some() {
                        active.codec = binfo.codec;
                    }
                    if !only_play || playing {
                        active.connected_seconds += elapsed;
                    }
                    if playing {
                        active.listening_seconds += elapsed;
                    }
                    active.connected_human = format_short(active.connected_seconds);
                    active.listening_human = format_short(active.listening_seconds);
                    if let Some(bat) = binfo.battery {
                        if active.battery_start.is_none() {
                            active.battery_start = Some(bat);
                        }
                        active.battery_end = Some(bat);
                    }
                }
            } else if let Some(last) = dev.sessions.last_mut() {
                if last.status == "active" {
                    last.status = "completed".into();
                    last.end = now_iso.clone();
                }
            }
            Self::update_totals(dev);
        }
        self.save_db();
    }
}

// ==================== MODO TUI (RATATUI) ====================
fn run_tui(mut tracker: Tracker) -> Result<(), Box<dyn std::error::Error>> {
    enable_raw_mode()?;
    let mut stdout = stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut last_tick = Instant::now();

    loop {
        let poll = Duration::from_secs(tracker.db.settings.poll_interval.max(1));
        if last_tick.elapsed() >= poll {
            tracker.tick(false);
            last_tick = Instant::now();
        }

        terminal.draw(|f| {
            let s = &tracker.db.settings;
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .margin(1)
                .constraints([Constraint::Length(3), Constraint::Min(5)].as_ref())
                .split(f.size());

            let header_text = format!(
                "⚙️ Intervalo: {}s [+/-] | Segundos: {} [s] | Solo Reprod: {} [p] | Códec: {} [c] | Salir [q]",
                s.poll_interval,
                if s.show_seconds { "SÍ" } else { "NO" },
                if s.only_playback { "SÍ" } else { "NO" },
                if s.show_codec { "SÍ" } else { "NO" }
            );
            let header = Paragraph::new(header_text)
                .style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))
                .block(Block::default().borders(Borders::ALL).title(" 🎧 YLIH-RS (Rust TUI) "));
            f.render_widget(header, chunks[0]);

            let mut items = Vec::new();
            for (mac, dev) in &tracker.db.devices {
                let status = if dev.currently_connected { "🟢 CONECTADO" } else { "⚪ DESCONECTADO" };
                let codec = if s.show_codec && dev.currently_connected {
                    format!(" [🎼 {}]", dev.current_codec.as_deref().unwrap_or("SBC/AAC"))
                } else {
                    String::new()
                };
                items.push(ListItem::new(Line::from(vec![
                    Span::styled(format!("🎧 {} ({}) — {}{}", dev.name, mac, status, codec), Style::default().add_modifier(Modifier::BOLD)),
                ])));
                if !s.only_playback {
                    items.push(ListItem::new(format!("   ⏱️  Conectado Total : {}", format_time(dev.lifetime_connected_seconds, s.show_seconds))));
                }
                items.push(ListItem::new(format!("   🎵 Reproduciendo   : {}", format_time(dev.lifetime_listening_seconds, s.show_seconds))));
                if let Some(last) = dev.sessions.last() {
                    let bat = dev.current_battery.or(last.battery_end).unwrap_or(0);
                    items.push(ListItem::new(format!(
                        "   🔋 Batería: {}%  |  📊 Última sesión: {} conectado / {} sonando [{}]",
                        bat, last.connected_human, last.listening_human, last.status
                    )));
                }
                items.push(ListItem::new("─".repeat(70)));
            }
            let list = List::new(items).block(Block::default().borders(Borders::ALL).title(" Dispositivos Bluetooth "));
            f.render_widget(list, chunks[1]);
        })?;

        if event::poll(Duration::from_millis(250))? {
            if let Event::Key(key) = event::read()? {
                match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => break,
                    KeyCode::Char('s') => { tracker.db.settings.show_seconds = !tracker.db.settings.show_seconds; tracker.save_db(); }
                    KeyCode::Char('p') => { tracker.db.settings.only_playback = !tracker.db.settings.only_playback; tracker.save_db(); }
                    KeyCode::Char('c') => { tracker.db.settings.show_codec = !tracker.db.settings.show_codec; tracker.save_db(); }
                    KeyCode::Char('+') | KeyCode::Char('=') => { tracker.db.settings.poll_interval = (tracker.db.settings.poll_interval + 1).min(60); tracker.save_db(); }
                    KeyCode::Char('-') => { tracker.db.settings.poll_interval = tracker.db.settings.poll_interval.saturating_sub(1).max(1); tracker.save_db(); }
                    KeyCode::Char('r') => tracker.tick(false),
                    _ => {}
                }
            }
        }
    }

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    Ok(())
}

// ==================== MODO GTK3 NATIVO ====================
fn run_gtk(tracker: Tracker) {
    gtk::init().expect("Error inicializando GTK3");
    let tracker = Rc::new(RefCell::new(tracker));

    let win = gtk::Window::new(gtk::WindowType::Toplevel);
    win.set_title("YLIH - Your Life in Headphones");
    win.set_default_size(580, 480);
    win.set_border_width(10);

    let notebook = gtk::Notebook::new();
    win.add(&notebook);

    // Pestaña 1: Dispositivos
    let tab_monitor = gtk::Box::new(gtk::Orientation::Vertical, 8);
    tab_monitor.set_border_width(10);
    notebook.append_page(&tab_monitor, Some(&gtk::Label::new(Some("🎧 Dispositivos"))));

    let scroll = gtk::ScrolledWindow::new(gtk::Adjustment::NONE, gtk::Adjustment::NONE);
    scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    tab_monitor.pack_start(&scroll, true, true, 0);

    let content_box = gtk::Box::new(gtk::Orientation::Vertical, 12);
    scroll.add(&content_box);

    // Pestaña 2: Ajustes
    let tab_settings = gtk::Box::new(gtk::Orientation::Vertical, 12);
    tab_settings.set_border_width(16);
    notebook.append_page(&tab_settings, Some(&gtk::Label::new(Some("⚙️ Ajustes"))));

    let grid = gtk::Grid::new();
    grid.set_column_spacing(20);
    grid.set_row_spacing(14);
    tab_settings.pack_start(&grid, false, false, 0);

    let s = tracker.borrow().db.settings.clone();

    let lbl_poll = gtk::Label::new(None);
    lbl_poll.set_markup("<b>⏱️ Intervalo de actualización (segundos):</b>");
    lbl_poll.set_xalign(0.0);
    let adj = gtk::Adjustment::new(s.poll_interval as f64, 1.0, 60.0, 1.0, 5.0, 0.0);
    let spin = gtk::SpinButton::new(Some(&adj), 1.0, 0);
    grid.attach(&lbl_poll, 0, 0, 1, 1);
    grid.attach(&spin, 1, 0, 1, 1);

    let lbl_sec = gtk::Label::new(None);
    lbl_sec.set_markup("<b>🔢 Mostrar segundos exactos:</b>");
    lbl_sec.set_xalign(0.0);
    let sw_sec = gtk::Switch::new();
    sw_sec.set_active(s.show_seconds);
    sw_sec.set_halign(gtk::Align::Start);
    grid.attach(&lbl_sec, 0, 1, 1, 1);
    grid.attach(&sw_sec, 1, 1, 1, 1);

    let lbl_codec = gtk::Label::new(None);
    lbl_codec.set_markup("<b>🎼 Mostrar códec Bluetooth (AAC/SBC):</b>");
    lbl_codec.set_xalign(0.0);
    let sw_codec = gtk::Switch::new();
    sw_codec.set_active(s.show_codec);
    sw_codec.set_halign(gtk::Align::Start);
    grid.attach(&lbl_codec, 0, 2, 1, 1);
    grid.attach(&sw_codec, 1, 2, 1, 1);

    let lbl_only = gtk::Label::new(None);
    lbl_only.set_markup("<b>🎵 Contar solo tiempo de reproducción:</b>");
    lbl_only.set_xalign(0.0);
    let sw_only = gtk::Switch::new();
    sw_only.set_active(s.only_playback);
    sw_only.set_halign(gtk::Align::Start);
    grid.attach(&lbl_only, 0, 3, 1, 1);
    grid.attach(&sw_only, 1, 3, 1, 1);

    let lbl_sysd = gtk::Label::new(None);
    lbl_sysd.set_markup("<b>🚀 Servicio automático (systemd --user):</b>");
    lbl_sysd.set_xalign(0.0);
    let sw_sysd = gtk::Switch::new();
    sw_sysd.set_active(is_systemd_enabled());
    sw_sysd.set_halign(gtk::Align::Start);
    grid.attach(&lbl_sysd, 0, 4, 1, 1);
    grid.attach(&sw_sysd, 1, 4, 1, 1);

    let btn_import = gtk::Button::with_label("📥 Importar y fusionar log (.json)");
    tab_settings.pack_start(&btn_import, false, false, 8);

    let refresh_ui: Rc<dyn Fn()> = {
        let content_box = content_box.clone();
        let tracker = tracker.clone();
        Rc::new(move || {
            for child in content_box.children() {
                content_box.remove(&child);
            }
            let t = tracker.borrow();
            let s = &t.db.settings;

            for (mac, dev) in &t.db.devices {
                let status = if dev.currently_connected { "🟢 CONECTADO" } else { "⚪ DESCONECTADO" };
                let codec = if s.show_codec && dev.currently_connected {
                    format!("  |  🎼 <b>{}</b>", dev.current_codec.as_deref().unwrap_or("SBC/AAC"))
                } else {
                    String::new()
                };

                let frame = gtk::Frame::new(None);
                let flbl = gtk::Label::new(None);
                flbl.set_markup(&format!(
                    "<b>🎧 {}</b> <small>({})</small>  —  <b>{}</b>{}",
                    glib::markup_escape_text(&dev.name), mac, status, codec
                ));
                frame.set_label_widget(Some(&flbl));

                let card = gtk::Box::new(gtk::Orientation::Vertical, 8);
                card.set_border_width(12);
                frame.add(&card);

                if !s.only_playback {
                    let l1 = gtk::Label::new(None);
                    l1.set_markup(&format!("<b>⏱️ Tiempo Conectado Total:</b>  {}", format_time(dev.lifetime_connected_seconds, s.show_seconds)));
                    l1.set_xalign(0.0);
                    card.pack_start(&l1, false, false, 0);
                }

                let l2 = gtk::Label::new(None);
                l2.set_markup(&format!("<b>🎵 Tiempo Reproduciendo:</b>  {}", format_time(dev.lifetime_listening_seconds, s.show_seconds)));
                l2.set_xalign(0.0);
                card.pack_start(&l2, false, false, 0);

                if let Some(bat) = dev.current_battery.or_else(|| dev.sessions.last().and_then(|x| x.battery_end)) {
                    let b_box = gtk::Box::new(gtk::Orientation::Horizontal, 10);
                    let blbl = gtk::Label::new(None);
                    blbl.set_markup(&format!("<b>🔋 Batería ({}%):</b>", bat));
                    let bar = gtk::ProgressBar::new();
                    bar.set_fraction((bat as f64 / 100.0).clamp(0.0, 1.0));
                    b_box.pack_start(&blbl, false, false, 0);
                    b_box.pack_start(&bar, true, true, 0);
                    card.pack_start(&b_box, false, false, 4);
                }

                for sess in dev.sessions.iter().rev().take(5) {
                    let b1 = sess.battery_start.map(|b| format!("{}%", b)).unwrap_or_else(|| "?".into());
                    let b2 = sess.battery_end.map(|b| format!("{}%", b)).unwrap_or_else(|| "?".into());
                    let row = gtk::Label::new(Some(&format!(
                        "📅 {}  |  ⏱️ {}  |  🎵 {}  |  🔋 {} ➔ {}  [{}]",
                        &sess.start.replace('T', " "), sess.connected_human, sess.listening_human, b1, b2, sess.status
                    )));
                    row.set_xalign(0.0);
                    card.pack_start(&row, false, false, 2);
                }

                content_box.pack_start(&frame, false, false, 0);
            }
            content_box.show_all();
        })
    };

    // Eventos de los controles de ajustes
    {
        let tracker = tracker.clone();
        spin.connect_value_changed(move |sp| {
            tracker.borrow_mut().db.settings.poll_interval = sp.value_as_int() as u64;
            tracker.borrow().save_db();
        });
    }
    {
        let tracker = tracker.clone();
        let refresh = refresh_ui.clone();
        sw_sec.connect_state_set(move |_, state| {
            tracker.borrow_mut().db.settings.show_seconds = state;
            tracker.borrow().save_db();
            refresh();
            glib::Propagation::Proceed
        });
    }
    {
        let tracker = tracker.clone();
        let refresh = refresh_ui.clone();
        sw_codec.connect_state_set(move |_, state| {
            tracker.borrow_mut().db.settings.show_codec = state;
            tracker.borrow().save_db();
            refresh();
            glib::Propagation::Proceed
        });
    }
    {
        let tracker = tracker.clone();
        let refresh = refresh_ui.clone();
        sw_only.connect_state_set(move |_, state| {
            tracker.borrow_mut().db.settings.only_playback = state;
            tracker.borrow().save_db();
            refresh();
            glib::Propagation::Proceed
        });
    }
    sw_sysd.connect_state_set(move |_, state| {
        set_systemd_service(state);
        glib::Propagation::Proceed
    });

    {
        let win_weak = win.downgrade();
        let tracker = tracker.clone();
        let refresh = refresh_ui.clone();
        btn_import.connect_clicked(move |_| {
            let Some(win) = win_weak.upgrade() else { return };
            let dlg = gtk::FileChooserDialog::with_buttons(
                Some("Importar ylih_log.json"),
                Some(&win),
                gtk::FileChooserAction::Open,
                &[("Cancelar", gtk::ResponseType::Cancel), ("Abrir", gtk::ResponseType::Accept)],
            );
            if dlg.run() == gtk::ResponseType::Accept {
                if let Some(path) = dlg.filename() {
                    if let Some(p_str) = path.to_str() {
                        let _ = tracker.borrow_mut().import_external_db(p_str);
                        refresh();
                    }
                }
            }
            unsafe { dlg.destroy(); }
        });
    }

    refresh_ui();

    // Ciclo de actualización reactivo en GTK
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

fn main() {
    let args: Vec<String> = env::args().collect();
    let mut tracker = Tracker::new();

    if args.iter().any(|a| a == "--daemon") {
        loop {
            let poll = tracker.db.settings.poll_interval.max(1);
            std::thread::sleep(Duration::from_secs(poll));
            tracker.tick(false);
        }
    } else if args.iter().any(|a| a == "--tui") {
        let _ = run_tui(tracker);
    } else {
        run_gtk(tracker);
    }
}
