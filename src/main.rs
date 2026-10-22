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
    cell::RefCell,
    collections::{BTreeMap, HashMap, HashSet},
    env, fs,
    io::stdout,
    path::PathBuf,
    process::Command,
    rc::Rc,
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
struct TelemetryPoint {
    t_sec: u64,
    battery: Option<u8>,
    volume: Option<u8>,
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
    #[serde(default)]
    volume_start: Option<u8>,
    #[serde(default)]
    volume_avg: Option<u8>,
    #[serde(default)]
    volume_max: Option<u8>,
    #[serde(default)]
    volume_increased: bool,
    #[serde(default)]
    samples: Vec<TelemetryPoint>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
struct DeviceRecord {
    name: String,
    #[serde(default)]
    real_name: String,
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
    #[serde(default)]
    current_volume: Option<u8>,
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
    alias: String,
    real_name: String,
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

fn get_current_volume() -> Option<u8> {
    if let Ok(out) = Command::new("wpctl")
        .args(["get-volume", "@DEFAULT_AUDIO_SINK@"])
        .output()
        {
            let s = String::from_utf8_lossy(&out.stdout);
            // Ejemplo de salida: "Volume: 0.86" o "Volume: 0.86 [MUTED]"
            for part in s.split_whitespace() {
                if let Ok(val) = part.parse::<f32>() {
                    return Some((val * 100.0).round().clamp(0.0, 150.0) as u8);
                }
            }
        }
        None
}

fn get_pipewire_codecs() -> HashMap<String, String> {
    let mut map = HashMap::new();
    if let Ok(out) = Command::new("pw-dump").output() {
        if let Ok(json) = serde_json::from_slice::<serde_json::Value>(&out.stdout) {
            if let Some(arr) = json.as_array() {
                for obj in arr {
                    if let Some(props) = obj.pointer("/info/props") {
                        let mac = props
                        .get("api.bluez5.address")
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
    ) else {
        return result;
    };

    type ManagedObjects = HashMap<
    zbus::zvariant::OwnedObjectPath,
    HashMap<String, HashMap<String, OwnedValue>>,
    >;

    let Ok(objects) = reply.body().deserialize::<ManagedObjects>() else {
        return result;
    };

    for (_path, ifaces) in objects {
        let Some(dev) = ifaces.get("org.bluez.Device1") else { continue };

        let icon = dev
        .get("Icon")
        .and_then(|v| <&str>::try_from(v).ok())
        .unwrap_or("");
        let mut is_audio =
        icon.contains("audio") || icon.contains("headset") || icon.contains("headphone");

        if let Some(uuids_val) = dev.get("UUIDs") {
            let uuids_str = format!("{:?}", uuids_val).to_lowercase();
            if AUDIO_UUIDS.iter().any(|u| uuids_str.contains(u)) {
                is_audio = true;
            }
        }

        if !is_audio {
            continue;
        }

        let mac = dev
        .get("Address")
        .and_then(|v| <&str>::try_from(v).ok())
        .unwrap_or("00:00:00:00:00:00")
        .to_uppercase();

        let real_name = dev
        .get("Name")
        .and_then(|v| <&str>::try_from(v).ok())
        .unwrap_or(&mac)
        .to_string();

        let alias = dev
        .get("Alias")
        .and_then(|v| <&str>::try_from(v).ok())
        .unwrap_or(&real_name)
        .to_string();

        let connected = dev
        .get("Connected")
        .and_then(|v| bool::try_from(v).ok())
        .unwrap_or(false);

        let battery = ifaces
        .get("org.bluez.Battery1")
        .and_then(|b| b.get("Percentage"))
        .and_then(|v| u8::try_from(v).ok());

        let codec = if connected {
            Some(
                pw_codecs
                .get(&mac)
                .cloned()
                .unwrap_or_else(|| "AAC/SBC".to_string()),
            )
        } else {
            None
        };

        result.insert(
            mac,
            BluezInfo {
                alias,
                real_name,
                connected,
                battery,
                codec,
            },
        );
    }
    result
}

fn is_audio_playing() -> bool {
    Command::new("playerctl")
    .args(["-a", "status"])
    .output()
    .map(|o| {
        String::from_utf8_lossy(&o.stdout)
        .lines()
        .any(|l| l.trim() == "Playing")
    })
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
        let _ = Command::new("systemctl")
        .args(["--user", "daemon-reload"])
        .status();
        let _ = Command::new("systemctl")
        .args(["--user", "enable", "--now", "ylih.service"])
        .status();
    } else {
        let _ = Command::new("systemctl")
        .args(["--user", "disable", "--now", "ylih.service"])
        .status();
    }
}

struct Tracker {
    db: Database,
    last_tick: Instant,
}

impl Tracker {
    fn new(fast_ui_start: bool) -> Self {
        let mut t = Self {
            db: Self::load_db(),
            last_tick: Instant::now(),
        };
        if !fast_ui_start {
            t.clean_orphan_sessions();
            t.tick(true);
        }
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
                let existing: HashSet<String> =
                local_dev.sessions.iter().map(|s| s.start.clone()).collect();
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
        let elapsed = if initial {
            0
        } else {
            self.last_tick.elapsed().as_secs()
        };
        self.last_tick = Instant::now();
        let now_iso = Local::now().format("%Y-%m-%dT%H:%M:%S").to_string();

        let bluez = query_bluez_devices();
        let playing = is_audio_playing();
        let current_vol = get_current_volume();
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
                name: binfo.alias.clone(),
                                                                real_name: binfo.real_name.clone(),
                                                                lifetime_connected_seconds: 0,
                                                                lifetime_listening_seconds: 0,
                                                                lifetime_connected_human: "0m 00s".into(),
                                                                lifetime_listening_human: "0m 00s".into(),
                                                                sessions: Vec::new(),
                                                                currently_connected: binfo.connected,
                                                                current_battery: binfo.battery,
                                                                current_codec: binfo.codec.clone(),
                                                                current_volume: current_vol,
            });

            dev.name = binfo.alias;
            if !binfo.real_name.is_empty() {
                dev.real_name = binfo.real_name;
            }
            dev.currently_connected = binfo.connected;
            dev.current_battery = binfo.battery;
            dev.current_codec = binfo.codec.clone();
            dev.current_volume = current_vol;

            if binfo.connected {
                let is_active = dev
                .sessions
                .last()
                .map(|s| s.status == "active")
                .unwrap_or(false);
                if !is_active {
                    let initial_point = TelemetryPoint {
                        t_sec: 0,
                        battery: binfo.battery,
                        volume: current_vol,
                    };
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
                                      volume_start: current_vol,
                                      volume_avg: current_vol,
                                      volume_max: current_vol,
                                      volume_increased: false,
                                      samples: vec![initial_point],
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

                    if let Some(vol) = current_vol {
                        let v_start = *active.volume_start.get_or_insert(vol);
                        if vol > v_start {
                            active.volume_increased = true;
                        }
                        let v_max = active.volume_max.unwrap_or(vol).max(vol);
                        active.volume_max = Some(v_max);
                    }

                    // Guardar muestra en la gráfica si cambió batería/volumen o cada 30s
                    let should_sample = active
                    .samples
                    .last()
                    .map(|last_p| {
                        active.connected_seconds.saturating_sub(last_p.t_sec) >= 30
                        || last_p.battery != binfo.battery
                        || last_p.volume != current_vol
                    })
                    .unwrap_or(true);

                    if should_sample && active.samples.len() < 720 {
                        active.samples.push(TelemetryPoint {
                            t_sec: active.connected_seconds,
                            battery: binfo.battery,
                            volume: current_vol,
                        });
                    }

                    let vols: Vec<u64> = active
                    .samples
                    .iter()
                    .filter_map(|p| p.volume.map(|v| v as u64))
                    .collect();
                    if !vols.is_empty() {
                        let avg = (vols.iter().sum::<u64>() / vols.len() as u64) as u8;
                        active.volume_avg = Some(avg);
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

// ==================== HELPER DE ÍCONOS DEL TEMA GTK ====================
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

// ==================== VENTANA EMERGENTE DE GRÁFICA Y DETALLES ====================
fn show_session_details_dialog(
    parent: &gtk::Window,
    mac: &str,
    dev: &DeviceRecord,
    sess: &Session,
) {
    let dlg = gtk::Dialog::with_buttons(
        Some("Detalles de Sesión y Curva de Descarga"),
                                        Some(parent),
                                        gtk::DialogFlags::MODAL | gtk::DialogFlags::DESTROY_WITH_PARENT,
                                        &[("Cerrar", gtk::ResponseType::Close)],
    );
    dlg.set_default_size(560, 440);
    dlg.set_border_width(12);

    let area = dlg.content_area();
    area.set_spacing(10);

    let real_model = if dev.real_name.is_empty() {
        &dev.name
    } else {
        &dev.real_name
    };

    let info_frame = gtk::Frame::new(Some("Identificación del Dispositivo y Telemetría"));
    let info_box = gtk::Box::new(gtk::Orientation::Vertical, 6);
    info_box.set_border_width(10);
    info_frame.add(&info_box);

    info_box.pack_start(
        &icon_label_row(
            "audio-headphones-symbolic",
            &format!(
                "<b>Nombre personalizado (Alias):</b> {}   |   <b>Nombre real (Firmware):</b> {}",
                     glib::markup_escape_text(&dev.name),
                     glib::markup_escape_text(real_model)
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
                "<b>Dirección MAC:</b> <tt>{}</tt>   |   <b>Códec:</b> {}",
                mac,
                sess.codec.as_deref().unwrap_or("AAC/SBC")
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
        "Sí (el usuario subió el volumen durante la sesión)"
    } else {
        "No (se mantuvo estable o bajó)"
    };

    info_box.pack_start(
        &icon_label_row(
            "multimedia-volume-control-symbolic",
            &format!(
                "<b>Volumen de sesión:</b> Inicial: <b>{}</b>  |  Promedio: <b>{}</b>  |  Pico Máx: <b>{}</b>\n<b>¿Aumento de volumen detectado?:</b> {}",
                v_start, v_avg, v_max, v_inc_txt
            ),
        ),
        false,
        false,
        0,
    );

    let b1 = sess
    .battery_start
    .map(|b| format!("{}%", b))
    .unwrap_or_else(|| "?".into());
    let b2 = sess
    .battery_end
    .map(|b| format!("{}%", b))
    .unwrap_or_else(|| "?".into());

    info_box.pack_start(
        &icon_label_row(
            "battery-good-symbolic",
            &format!(
                "<b>Tiempo Conectado:</b> {}   |   <b>Reproduciendo:</b> {}   |   <b>Batería:</b> {} ➔ {}",
                sess.connected_human, sess.listening_human, b1, b2
            ),
        ),
        false,
        false,
        0,
    );

    area.pack_start(&info_frame, false, false, 0);

    // Lienzo Cairo para dibujar la gráfica de batería y volumen
    let graph_frame = gtk::Frame::new(Some(
        "Gráfica de Sesión: Batería % (Verde) vs Volumen % (Azul)",
    ));
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

    drawing.connect_draw(move |widget, cr| {
        let w = widget.allocated_width() as f64;
        let h = widget.allocated_height() as f64;
        let pad_l = 42.0;
        let pad_r = 20.0;
        let pad_t = 18.0;
        let pad_b = 28.0;
        let gw = (w - pad_l - pad_r).max(10.0);
        let gh = (h - pad_t - pad_b).max(10.0);

        // Fondo oscuro elegante
        cr.set_source_rgb(0.11, 0.12, 0.14);
        let _ = cr.paint();

        // Líneas guía horizontales (100%, 75%, 50%, 25%, 0%)
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

        // Curva de Batería (Verde)
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
            cr.set_source_rgb(0.30, 0.85, 0.45);
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

        // Curva de Volumen (Azul Cian)
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
            cr.set_source_rgb(0.35, 0.70, 0.98);
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

        // Etiquetas de tiempo en eje X
        cr.set_source_rgba(0.85, 0.85, 0.85, 0.85);
        cr.move_to(pad_l, h - 8.0);
        let _ = cr.show_text("Inicio (0s)");
        let end_lbl = format!("Fin ({})", format_short(max_t as u64));
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

// ==================== MODO TUI (RATATUI) ====================
fn run_tui(mut tracker: Tracker) -> Result<(), Box<dyn std::error::Error>> {
    tracker.clean_orphan_sessions();
    tracker.tick(true);

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
                "Intervalo: {}s [+/-] | Segundos: {} [s] | Solo Reprod: {} [p] | Códec: {} [c] | Salir [q]",
                s.poll_interval,
                if s.show_seconds { "SÍ" } else { "NO" },
                    if s.only_playback { "SÍ" } else { "NO" },
                        if s.show_codec { "SÍ" } else { "NO" }
            );
            let header = Paragraph::new(header_text)
            .style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))
            .block(Block::default().borders(Borders::ALL).title(" YLIH-RS v2.1 (Rust TUI) "));
            f.render_widget(header, chunks[0]);

            let mut items = Vec::new();
            for (mac, dev) in &tracker.db.devices {
                let status = if dev.currently_connected { "CONECTADO" } else { "DESCONECTADO" };
                let codec = if s.show_codec && dev.currently_connected {
                    format!(" [{}]", dev.current_codec.as_deref().unwrap_or("SBC/AAC"))
                } else {
                    String::new()
                };
                let real_info = if !dev.real_name.is_empty() && dev.real_name != dev.name {
                    format!(" [Modelo: {}]", dev.real_name)
                } else {
                    String::new()
                };
                items.push(ListItem::new(Line::from(vec![Span::styled(
                    format!("{} ({}){} — {}{}", dev.name, mac, real_info, status, codec),
                        Style::default().add_modifier(Modifier::BOLD),
                )])));
                if !s.only_playback {
                    items.push(ListItem::new(format!(
                        "   Conectado Total : {}",
                        format_time(dev.lifetime_connected_seconds, s.show_seconds)
                    )));
                }
                items.push(ListItem::new(format!(
                    "   Reproduciendo   : {}",
                    format_time(dev.lifetime_listening_seconds, s.show_seconds)
                )));
                if let Some(last) = dev.sessions.last() {
                    let bat = dev.current_battery.or(last.battery_end).unwrap_or(0);
                    let vol = dev.current_volume.or(last.volume_avg).unwrap_or(0);
                    items.push(ListItem::new(format!(
                        "   Batería: {}% | Vol: {}% | Última sesión: {} conectado / {} sonando [{}]",
                        bat, vol, last.connected_human, last.listening_human, last.status
                    )));
                }
                items.push(ListItem::new("─".repeat(72)));
            }
            let list = List::new(items)
            .block(Block::default().borders(Borders::ALL).title(" Dispositivos Bluetooth "));
            f.render_widget(list, chunks[1]);
        })?;

        if event::poll(Duration::from_millis(250))? {
            if let Event::Key(key) = event::read()? {
                match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => break,
                    KeyCode::Char('s') => {
                        tracker.db.settings.show_seconds = !tracker.db.settings.show_seconds;
                        tracker.save_db();
                    }
                    KeyCode::Char('p') => {
                        tracker.db.settings.only_playback = !tracker.db.settings.only_playback;
                        tracker.save_db();
                    }
                    KeyCode::Char('c') => {
                        tracker.db.settings.show_codec = !tracker.db.settings.show_codec;
                        tracker.save_db();
                    }
                    KeyCode::Char('+') | KeyCode::Char('=') => {
                        tracker.db.settings.poll_interval =
                        (tracker.db.settings.poll_interval + 1).min(60);
                        tracker.save_db();
                    }
                    KeyCode::Char('-') => {
                        tracker.db.settings.poll_interval =
                        tracker.db.settings.poll_interval.saturating_sub(1).max(1);
                        tracker.save_db();
                    }
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

// ==================== MODO GTK3 NATIVO CON ÍCONOS DEL SISTEMA ====================
fn run_gtk(tracker: Tracker) {
    gtk::init().expect("Error inicializando GTK3");
    let tracker = Rc::new(RefCell::new(tracker));

    let win = gtk::Window::new(gtk::WindowType::Toplevel);
    win.set_title("YLIH - Your Life in Headphones");
    win.set_default_size(640, 520);
    win.set_border_width(10);
    win.set_icon_name(Some("audio-headphones"));

    let notebook = gtk::Notebook::new();
    win.add(&notebook);

    // Pestaña 1: Dispositivos (con ícono del sistema)
    let tab_monitor = gtk::Box::new(gtk::Orientation::Vertical, 8);
    tab_monitor.set_border_width(10);
    let tab1_hdr = icon_label_row("audio-headphones-symbolic", "Dispositivos");
    tab1_hdr.show_all();
    notebook.append_page(&tab_monitor, Some(&tab1_hdr));

    let scroll = gtk::ScrolledWindow::new(gtk::Adjustment::NONE, gtk::Adjustment::NONE);
    scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    tab_monitor.pack_start(&scroll, true, true, 0);

    let content_box = gtk::Box::new(gtk::Orientation::Vertical, 12);
    scroll.add(&content_box);

    // Pestaña 2: Ajustes (con ícono del sistema)
    let tab_settings = gtk::Box::new(gtk::Orientation::Vertical, 12);
    tab_settings.set_border_width(16);
    let tab2_hdr = icon_label_row("preferences-system-symbolic", "Ajustes");
    tab2_hdr.show_all();
    notebook.append_page(&tab_settings, Some(&tab2_hdr));

    let grid = gtk::Grid::new();
    grid.set_column_spacing(20);
    grid.set_row_spacing(14);
    tab_settings.pack_start(&grid, false, false, 0);

    let s = tracker.borrow().db.settings.clone();

    let row_poll = icon_label_row(
        "preferences-system-time-symbolic",
        "<b>Intervalo de actualización (segundos):</b>",
    );
    let adj = gtk::Adjustment::new(s.poll_interval as f64, 1.0, 60.0, 1.0, 5.0, 0.0);
    let spin = gtk::SpinButton::new(Some(&adj), 1.0, 0);
    grid.attach(&row_poll, 0, 0, 1, 1);
    grid.attach(&spin, 1, 0, 1, 1);

    let row_sec = icon_label_row("view-list-symbolic", "<b>Mostrar segundos exactos:</b>");
    let sw_sec = gtk::Switch::new();
    sw_sec.set_active(s.show_seconds);
    sw_sec.set_halign(gtk::Align::Start);
    grid.attach(&row_sec, 0, 1, 1, 1);
    grid.attach(&sw_sec, 1, 1, 1, 1);

    let row_codec = icon_label_row(
        "audio-card-symbolic",
        "<b>Mostrar códec Bluetooth (AAC/SBC):</b>",
    );
    let sw_codec = gtk::Switch::new();
    sw_codec.set_active(s.show_codec);
    sw_codec.set_halign(gtk::Align::Start);
    grid.attach(&row_codec, 0, 2, 1, 1);
    grid.attach(&sw_codec, 1, 2, 1, 1);

    let row_only = icon_label_row(
        "media-playback-start-symbolic",
        "<b>Contar solo tiempo de reproducción:</b>",
    );
    let sw_only = gtk::Switch::new();
    sw_only.set_active(s.only_playback);
    sw_only.set_halign(gtk::Align::Start);
    grid.attach(&row_only, 0, 3, 1, 1);
    grid.attach(&sw_only, 1, 3, 1, 1);

    let row_sysd = icon_label_row(
        "system-run-symbolic",
        "<b>Servicio automático (systemd --user):</b>",
    );
    let sw_sysd = gtk::Switch::new();
    sw_sysd.set_active(is_systemd_enabled());
    sw_sysd.set_halign(gtk::Align::Start);
    grid.attach(&row_sysd, 0, 4, 1, 1);
    grid.attach(&sw_sysd, 1, 4, 1, 1);

    let btn_import = gtk::Button::new();
    btn_import.add(&icon_label_row(
        "document-open-symbolic",
        "Importar y fusionar log (.json)",
    ));
    tab_settings.pack_start(&btn_import, false, false, 4);

    let btn_debug = gtk::Button::new();
    btn_debug.add(&icon_label_row(
        "utilities-terminal-symbolic",
        "Exportar paquete de depuración (Debug Log)",
    ));
    tab_settings.pack_start(&btn_debug, false, false, 4);

    let refresh_ui: Rc<dyn Fn()> = {
        let content_box = content_box.clone();
        let tracker = tracker.clone();
        let win_weak = win.downgrade();
        Rc::new(move || {
            for child in content_box.children() {
                content_box.remove(&child);
            }
            let t = tracker.borrow();
            let s = &t.db.settings;

            for (mac, dev) in &t.db.devices {
                let status_icon = if dev.currently_connected {
                    "network-bluetooth-activated-symbolic"
                } else {
                    "network-bluetooth-symbolic"
                };
                let status_txt = if dev.currently_connected {
                    "<span foreground='#4ade80'><b>CONECTADO</b></span>"
                } else {
                    "<span foreground='#9ca3af'><b>DESCONECTADO</b></span>"
                };
                let codec = if s.show_codec && dev.currently_connected {
                    format!(
                        "  |  <b>{}</b>",
                        dev.current_codec.as_deref().unwrap_or("SBC/AAC")
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
                        "<b>{}</b>{} <small>({})</small>  —  {}{}",
                             glib::markup_escape_text(&dev.name),
                             real_tag,
                             mac,
                             status_txt,
                             codec
                    ),
                );
                frame.set_label_widget(Some(&hdr_box));

                let card = gtk::Box::new(gtk::Orientation::Vertical, 8);
                card.set_border_width(12);
                frame.add(&card);

                if !s.only_playback {
                    card.pack_start(
                        &icon_label_row(
                            "preferences-system-time-symbolic",
                            &format!(
                                "<b>Tiempo Conectado Total:</b>  {}",
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
                            "<b>Tiempo Reproduciendo:</b>  {}",
                            format_time(dev.lifetime_listening_seconds, s.show_seconds)
                        ),
                    ),
                    false,
                    false,
                    0,
                );

                if let Some(bat) = dev
                    .current_battery
                    .or_else(|| dev.sessions.last().and_then(|x| x.battery_end))
                    {
                        let b_box = gtk::Box::new(gtk::Orientation::Horizontal, 10);
                        let bat_icon = if bat >= 80 {
                            "battery-full-symbolic"
                        } else if bat >= 40 {
                            "battery-good-symbolic"
                        } else {
                            "battery-low-symbolic"
                        };
                        let blbl = icon_label_row(bat_icon, &format!("<b>Batería ({}%):</b>", bat));
                        let bar = gtk::ProgressBar::new();
                        bar.set_fraction((bat as f64 / 100.0).clamp(0.0, 1.0));
                        b_box.pack_start(&blbl, false, false, 0);
                        b_box.pack_start(&bar, true, true, 0);
                        card.pack_start(&b_box, false, false, 4);
                    }

                    for sess in dev.sessions.iter().rev().take(6) {
                        let b1 = sess
                        .battery_start
                        .map(|b| format!("{}%", b))
                        .unwrap_or_else(|| "?".into());
                        let b2 = sess
                        .battery_end
                        .map(|b| format!("{}%", b))
                        .unwrap_or_else(|| "?".into());
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
                                "{}  |  Conectado: {}  |  Sonando: {}  |  Bat: {} ➔ {}{}  [{}]",
                                &sess.start.replace('T', " "),
                                     sess.connected_human,
                                     sess.listening_human,
                                     b1,
                                     b2,
                                     v_txt,
                                     sess.status
                            ),
                        );

                        let btn_details = gtk::Button::from_icon_name(
                            Some("utilities-system-monitor-symbolic"),
                                                                      gtk::IconSize::Button,
                        );
                        btn_details.set_tooltip_text(Some(
                            "Ver gráfica de descarga, volumen y detalles técnicos",
                        ));

                        let mac_cl = mac.clone();
                        let dev_cl = dev.clone();
                        let sess_cl = sess.clone();
                        let win_wk = win_weak.clone();
                        btn_details.connect_clicked(move |_| {
                            if let Some(w) = win_wk.upgrade() {
                                show_session_details_dialog(&w, &mac_cl, &dev_cl, &sess_cl);
                            }
                        });

                        s_row.pack_start(&info_lbl, true, true, 0);
                        s_row.pack_end(&btn_details, false, false, 0);
                        card.pack_start(&s_row, false, false, 2);
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
                                                           &[
                                                               ("Cancelar", gtk::ResponseType::Cancel),
                                                           ("Abrir", gtk::ResponseType::Accept),
                                                           ],
            );
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

    // Exportar logs de depuración
    {
        let win_weak = win.downgrade();
        btn_debug.connect_clicked(move |_| {
            let Some(win) = win_weak.upgrade() else { return };
            let dlg = gtk::FileChooserDialog::with_buttons(
                Some("Guardar reporte de depuración"),
                                                           Some(&win),
                                                           gtk::FileChooserAction::Save,
                                                           &[
                                                               ("Cancelar", gtk::ResponseType::Cancel),
                                                           ("Guardar", gtk::ResponseType::Accept),
                                                           ],
            );
            dlg.set_current_name(&format!(
                "ylih_debug_{}.log",
                Local::now().format("%Y%m%d_%H%M%S")
            ));

            if dlg.run() == gtk::ResponseType::Accept {
                if let Some(path) = dlg.filename() {
                    let mut report = String::new();
                    report.push_str("=== YLIH-RS v2.1 DEBUG REPORT ===\n");
                    report.push_str(&format!("Fecha: {}\n\n", Local::now()));

                    if let Ok(json_content) = fs::read_to_string(log_path()) {
                        report.push_str("--- YLIH JSON DATABASE ---\n");
                        report.push_str(&json_content);
                        report.push_str("\n\n");
                    }

                    if let Ok(out) = Command::new("systemctl")
                        .args(["--user", "status", "ylih.service"])
                        .output()
                        {
                            report.push_str("--- SYSTEMD USER SERVICE ---\n");
                            report.push_str(&String::from_utf8_lossy(&out.stdout));
                            report.push_str("\n\n");
                        }

                        if let Ok(out) = Command::new("wpctl").arg("status").output() {
                            report.push_str("--- WIREPLUMBER / PIPEWIRE STATUS ---\n");
                            report.push_str(&String::from_utf8_lossy(&out.stdout));
                            report.push_str("\n\n");
                        }

                        if let Ok(out) = Command::new("bluetoothctl").arg("info").output() {
                            report.push_str("--- BLUETOOTHCTL INFO ---\n");
                            report.push_str(&String::from_utf8_lossy(&out.stdout));
                        }

                        let _ = fs::write(&path, report);
                }
            }
            unsafe {
                dlg.destroy();
            }
        });
    }

    // Dibuja la ventana al instante con los datos del JSON
    refresh_ui();

    // Primer chequeo D-Bus/PipeWire justo después de abrir la ventana
    {
        let tracker = tracker.clone();
        let refresh = refresh_ui.clone();
        glib::timeout_add_local_once(Duration::from_millis(150), move || {
            tracker.borrow_mut().clean_orphan_sessions();
            tracker.borrow_mut().tick(true);
            refresh();
        });
    }

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
    let is_daemon = args.iter().any(|a| a == "--daemon");
    let mut tracker = Tracker::new(!is_daemon);

    if is_daemon {
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
