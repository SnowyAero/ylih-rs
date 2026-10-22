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
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    symbols,
    text::{Line, Span},
    widgets::{
        canvas::{Canvas, Line as CanvasLine},
        Block, Borders, Clear, List, ListItem, Paragraph, Tabs,
    },
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

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
struct Settings {
    #[serde(default = "default_poll")]
    poll_interval: u64,
    #[serde(default = "default_true")]
    show_seconds: bool,
    #[serde(default)]
    only_playback: bool,
    #[serde(default = "default_true")]
    show_codec: bool,
    #[serde(default = "default_true")]
    track_wired_usb: bool,
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
            track_wired_usb: true,
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
    #[serde(default = "default_conn_type")]
    conn_type: String, // "Bluetooth", "USB", "Jack 3.5mm"
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

fn default_conn_type() -> String {
    "Bluetooth".into()
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
struct Database {
    #[serde(default)]
    settings: Settings,
    #[serde(default)]
    devices: BTreeMap<String, DeviceRecord>,
}

#[derive(Clone, Debug)]
struct DetectedAudioDevice {
    alias: String,
    real_name: String,
    conn_type: String,
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
            for part in s.split_whitespace() {
                if let Ok(val) = part.parse::<f32>() {
                    return Some((val * 100.0).round().clamp(0.0, 150.0) as u8);
                }
            }
        }
        None
}

// Chequea si algún auricular USB inalámbrico reporta batería en /sys/class/power_supply
fn query_usb_headset_battery() -> Option<u8> {
    let entries = fs::read_dir("/sys/class/power_supply").ok()?;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_lowercase();
        // Ignorar baterías internas de laptop (BAT0/BAT1) y cargadores AC/ADP
        if name.starts_with("bat") || name.starts_with("ac") || name.starts_with("adp") || name.starts_with("ucsi") {
            continue;
        }
        let path = entry.path();
        let scope = fs::read_to_string(path.join("scope")).unwrap_or_default();
        let is_dev_battery = scope.trim().eq_ignore_ascii_case("Device")
        || name.contains("hid")
        || name.contains("headset")
        || name.contains("usb")
        || name.contains("wireless");
        if is_dev_battery {
            if let Ok(cap_str) = fs::read_to_string(path.join("capacity")) {
                if let Ok(pct) = cap_str.trim().parse::<u8>() {
                    return Some(pct.min(100));
                }
            }
        }
    }
    None
}

fn inspect_pipewire(
    track_wired_usb: bool,
) -> (HashMap<String, String>, BTreeMap<String, DetectedAudioDevice>) {
    let mut bt_codecs = HashMap::new();
    let mut wired_devs = BTreeMap::new();
    let usb_bat = query_usb_headset_battery();

    let Ok(out) = Command::new("pw-dump").output() else {
        return (bt_codecs, wired_devs);
    };
    let Ok(json) = serde_json::from_slice::<serde_json::Value>(&out.stdout) else {
        return (bt_codecs, wired_devs);
    };
    let Some(arr) = json.as_array() else {
        return (bt_codecs, wired_devs);
    };

    for obj in arr {
        if let Some(props) = obj.pointer("/info/props") {
            // 1. Códecs Bluetooth
            let mac = props
            .get("api.bluez5.address")
            .or_else(|| props.get("device.string"))
            .and_then(|v| v.as_str());
            let codec = props.get("api.bluez5.codec").and_then(|v| v.as_str());
            if let (Some(m), Some(c)) = (mac, codec) {
                bt_codecs.insert(m.to_uppercase(), c.to_uppercase());
            }

            if !track_wired_usb {
                continue;
            }

            let media_class = props
            .get("media.class")
            .and_then(|v| v.as_str())
            .unwrap_or("");
            if media_class == "Audio/Sink" {
                let bus = props
                .get("device.bus")
                .and_then(|v| v.as_str())
                .unwrap_or("");
                let node_name = props
                .get("node.name")
                .and_then(|v| v.as_str())
                .unwrap_or("");
                let desc = props
                .get("node.description")
                .or_else(|| props.get("node.nick"))
                .and_then(|v| v.as_str())
                .unwrap_or("Auriculares");
                let product = props
                .get("device.product.name")
                .or_else(|| props.get("alsa.card_name"))
                .and_then(|v| v.as_str())
                .unwrap_or(desc);

                // Detectar Cascos USB
                if bus.eq_ignore_ascii_case("usb") || node_name.to_lowercase().contains("usb") {
                    let serial = props
                    .get("device.serial")
                    .or_else(|| props.get("alsa.LongCardName"))
                    .and_then(|v| v.as_str())
                    .unwrap_or(node_name);
                    let id = format!("USB:{}", serial.chars().take(18).collect::<String>());
                    wired_devs.insert(
                        id,
                        DetectedAudioDevice {
                            alias: desc.to_string(),
                                      real_name: product.to_string(),
                                      conn_type: "USB".into(),
                                      connected: true,
                                      battery: usb_bat,
                                      codec: Some("PCM / USB".into()),
                        },
                    );
                }
            }
        }

        // 2. Detectar Jack 3.5mm en los puertos de la tarjeta de sonido (Device Port)
        if track_wired_usb {
            if let Some(routes) = obj.pointer("/info/params/Route").and_then(|v| v.as_array()) {
                for r in routes {
                    let r_name = r
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_lowercase();
                    let r_desc = r
                    .get("description")
                    .and_then(|v| v.as_str())
                    .unwrap_or("Auriculares Jack 3.5mm");
                    let avail = r
                    .get("available")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                    if (r_name.contains("headphone") || r_name.contains("headset"))
                        && avail != "no"
                        {
                            let card_name = obj
                            .pointer("/info/props/device.description")
                            .or_else(|| obj.pointer("/info/props/alsa.card_name"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("Audio Integrado");
                            wired_devs.insert(
                                "JACK:3.5MM".into(),
                                              DetectedAudioDevice {
                                                  alias: format!("{} ({})", r_desc, card_name),
                                              real_name: card_name.to_string(),
                                              conn_type: "Jack 3.5mm".into(),
                                              connected: true,
                                              battery: None, // Cable analógico no usa batería
                                              codec: Some("Analógico 3.5mm".into()),
                                              },
                            );
                        }
                }
            }
        }
    }

    (bt_codecs, wired_devs)
}

fn query_all_audio_devices(track_wired_usb: bool) -> BTreeMap<String, DetectedAudioDevice> {
    let (pw_codecs, mut devices) = inspect_pipewire(track_wired_usb);

    let Ok(conn) = Connection::system() else {
        return devices;
    };
    let Ok(reply) = conn.call_method(
        Some("org.bluez"),
                                     "/",
                                     Some("org.freedesktop.DBus.ObjectManager"),
                                     "GetManagedObjects",
                                     &(),
    ) else {
        return devices;
    };

    type ManagedObjects = HashMap<
    zbus::zvariant::OwnedObjectPath,
    HashMap<String, HashMap<String, OwnedValue>>,
    >;

    let Ok(objects) = reply.body().deserialize::<ManagedObjects>() else {
        return devices;
    };

    for (_path, ifaces) in objects {
        let Some(dev) = ifaces.get("org.bluez.Device1") else {
            continue;
        };

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

        devices.insert(
            mac,
            DetectedAudioDevice {
                alias,
                real_name,
                conn_type: "Bluetooth".into(),
                       connected,
                       battery,
                       codec,
            },
        );
    }
    devices
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
            "[Unit]\nDescription=YLIH Rust - Headphone Lifespan Daemon\nAfter=bluetooth.target pipewire.service\n\n[Service]\nExecStart=\"{}\" --daemon\nRestart=on-failure\nRestartSec=5\n\n[Install]\nWantedBy=default.target\n",
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

fn write_debug_log(path: &PathBuf) -> std::io::Result<()> {
    let mut report = String::new();
    report.push_str("=== YLIH-RS v2.1.1 DEBUG REPORT ===\n");
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

        fs::write(path, report)
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
        let detected = query_all_audio_devices(self.db.settings.track_wired_usb);
        for (id, dev) in self.db.devices.iter_mut() {
            let is_conn = detected.get(id).map(|b| b.connected).unwrap_or(false);
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

        let detected = query_all_audio_devices(self.db.settings.track_wired_usb);
        let playing = is_audio_playing();
        let current_vol = get_current_volume();
        let only_play = self.db.settings.only_playback;

        for (id, dev) in self.db.devices.iter_mut() {
            if !detected.contains_key(id) {
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

        for (id, info) in detected {
            let dev = self.db.devices.entry(id).or_insert_with(|| DeviceRecord {
                name: info.alias.clone(),
                                                               real_name: info.real_name.clone(),
                                                               conn_type: info.conn_type.clone(),
                                                               lifetime_connected_seconds: 0,
                                                               lifetime_listening_seconds: 0,
                                                               lifetime_connected_human: "0m 00s".into(),
                                                               lifetime_listening_human: "0m 00s".into(),
                                                               sessions: Vec::new(),
                                                               currently_connected: info.connected,
                                                               current_battery: info.battery,
                                                               current_codec: info.codec.clone(),
                                                               current_volume: current_vol,
            });

            dev.name = info.alias;
            if !info.real_name.is_empty() {
                dev.real_name = info.real_name;
            }
            dev.conn_type = info.conn_type;
            dev.currently_connected = info.connected;
            dev.current_battery = info.battery;
            dev.current_codec = info.codec.clone();
            dev.current_volume = current_vol;

            if info.connected {
                let is_active = dev
                .sessions
                .last()
                .map(|s| s.status == "active")
                .unwrap_or(false);
                if !is_active {
                    let initial_point = TelemetryPoint {
                        t_sec: 0,
                        battery: info.battery,
                        volume: current_vol,
                    };
                    dev.sessions.push(Session {
                        start: now_iso.clone(),
                                      end: now_iso.clone(),
                                      connected_seconds: 0,
                                      listening_seconds: 0,
                                      connected_human: "0m 00s".into(),
                                      listening_human: "0m 00s".into(),
                                      battery_start: info.battery,
                                      battery_end: info.battery,
                                      codec: info.codec,
                                      status: "active".into(),
                                      volume_start: current_vol,
                                      volume_avg: current_vol,
                                      volume_max: current_vol,
                                      volume_increased: false,
                                      samples: vec![initial_point],
                    });
                } else if let Some(active) = dev.sessions.last_mut() {
                    active.end = now_iso.clone();
                    if info.codec.is_some() {
                        active.codec = info.codec;
                    }
                    if !only_play || playing {
                        active.connected_seconds += elapsed;
                    }
                    if playing {
                        active.listening_seconds += elapsed;
                    }
                    active.connected_human = format_short(active.connected_seconds);
                    active.listening_human = format_short(active.listening_seconds);

                    if let Some(bat) = info.battery {
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

                    let should_sample = active
                    .samples
                    .last()
                    .map(|last_p| {
                        active.connected_seconds.saturating_sub(last_p.t_sec) >= 30
                        || last_p.battery != info.battery
                        || last_p.volume != current_vol
                    })
                    .unwrap_or(true);

                    if should_sample && active.samples.len() < 720 {
                        active.samples.push(TelemetryPoint {
                            t_sec: active.connected_seconds,
                            battery: info.battery,
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

// ==================== MODO TUI 1:1 (RATATUI CON GRÁFICA Y AJUSTES) ====================
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

fn run_tui(mut tracker: Tracker) -> Result<(), Box<dyn std::error::Error>> {
    tracker.clean_orphan_sessions();
    tracker.tick(true);

    enable_raw_mode()?;
    let mut stdout = stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut last_tick = Instant::now();
    let mut active_tab: usize = 0; // 0 = Dispositivos, 1 = Ajustes
    let mut selected_session_idx: usize = 0;
    let mut show_modal = false;
    let mut settings_cursor: usize = 0;
    let mut draft_settings = tracker.db.settings.clone();
    let mut draft_systemd = is_systemd_enabled();
    let mut status_msg = String::from("Listo. Usa [Tab] para cambiar pestaña, [↑/↓] y [Enter] para ver gráfica.");

    loop {
        let poll = Duration::from_secs(tracker.db.settings.poll_interval.max(1));
        if last_tick.elapsed() >= poll {
            tracker.tick(false);
            last_tick = Instant::now();
        }

        // Construir catálogo plano de sesiones visibles (máximo 6 por dispositivo, igual que GTK3)
        let mut flat_sessions: Vec<(String, DeviceRecord, Session)> = Vec::new();
        for (id, dev) in &tracker.db.devices {
            for sess in dev.sessions.iter().rev().take(6) {
                flat_sessions.push((id.clone(), dev.clone(), sess.clone()));
            }
        }
        if !flat_sessions.is_empty() && selected_session_idx >= flat_sessions.len() {
            selected_session_idx = flat_sessions.len() - 1;
        }

        terminal.draw(|f| {
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
                Line::from(" 🎧 1: Dispositivos "),
                      Line::from(" ⚙ 2: Ajustes "),
            ];
            let tabs = Tabs::new(tab_titles)
            .block(
                Block::default()
                .borders(Borders::ALL)
                .title(" YLIH-RS v2.1.1 (Paridad 1:1 GTK3 / TUI) "),
            )
            .select(active_tab)
            .style(Style::default().fg(Color::White))
            .highlight_style(
                Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
            );
            f.render_widget(tabs, chunks[0]);

            if active_tab == 0 {
                let s = &tracker.db.settings;
                let mut items = Vec::new();
                let mut current_flat_idx = 0;

                for (id, dev) in &tracker.db.devices {
                    let status = if dev.currently_connected {
                        "🟢 CONECTADO"
                    } else {
                        "⚪ DESCONECTADO"
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
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                    )])));

                    if !s.only_playback {
                        items.push(ListItem::new(format!(
                            "   ⏱ Tiempo Conectado Total : {}",
                            format_time(dev.lifetime_connected_seconds, s.show_seconds)
                        )));
                    }
                    items.push(ListItem::new(format!(
                        "   ▶ Tiempo Reproduciendo   : {}",
                        format_time(dev.lifetime_listening_seconds, s.show_seconds)
                    )));

                    if let Some(bat) = dev
                        .current_battery
                        .or_else(|| dev.sessions.last().and_then(|x| x.battery_end))
                        {
                            let filled = (bat as usize / 5).min(20);
                            let bar = format!(
                                "[{}{}] {}%",
                                "█".repeat(filled),
                                              "░".repeat(20 - filled),
                                              bat
                            );
                            items.push(ListItem::new(format!("   🔋 Batería               : {}", bar)));
                        } else {
                            items.push(ListItem::new(
                                "   🔌 Alimentación          : Por cable / Sin batería reportada",
                            ));
                        }

                        for sess in dev.sessions.iter().rev().take(6) {
                            let is_sel = current_flat_idx == selected_session_idx;
                            let prefix = if is_sel { "  ➤ [Enter: Gráfica] " } else { "    • " };
                            let b_str = match (sess.battery_start, sess.battery_end) {
                                (Some(b1), Some(b2)) => format!("Bat: {}%➔{}%", b1, b2),
                      _ => "Sin Bat".to_string(),
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
                                "{}{} | Conectado: {} | Sonando: {} | {}{} [{}]",
                                prefix,
                                sess.start.replace('T', " "),
                                                  sess.connected_human,
                                                  sess.listening_human,
                                                  b_str,
                                                  v_str,
                                                  sess.status
                            );
                            let style = if is_sel {
                                Style::default()
                                .fg(Color::Black)
                                .bg(Color::Cyan)
                                .add_modifier(Modifier::BOLD)
                            } else {
                                Style::default()
                            };
                            items.push(ListItem::new(Line::from(Span::styled(row_txt, style))));
                            current_flat_idx += 1;
                        }
                        items.push(ListItem::new("─".repeat(82)));
                }

                let list = List::new(items).block(
                    Block::default()
                    .borders(Borders::ALL)
                    .title(" Dispositivos (Bluetooth / USB / Jack 3.5mm) "),
                );
                f.render_widget(list, chunks[1]);
            } else {
                // Pestaña 2: Ajustes con Guardar, Revertir y Restaurar por defecto
                let has_unsaved = draft_settings != tracker.db.settings
                || draft_systemd != is_systemd_enabled();
                let unsaved_banner = if has_unsaved {
                    " ⚠ TIENES CAMBIOS SIN GUARDAR (Pulsa en 'Guardar cambios')"
                } else {
                    " ✔ Todos los ajustes están sincronizados"
                };

                let rows = vec![
                    format!(
                        "Intervalo de actualización (segundos)      : < {} s >",
                            draft_settings.poll_interval
                    ),
                    format!(
                        "Mostrar segundos exactos                   : [{}]",
                        if draft_settings.show_seconds { "ACTIVO" } else { "INACTIVO" }
                    ),
                    format!(
                        "Mostrar códec de audio (AAC/SBC/PCM)       : [{}]",
                            if draft_settings.show_codec { "ACTIVO" } else { "INACTIVO" }
                    ),
                    format!(
                        "Contar solo tiempo de reproducción         : [{}]",
                        if draft_settings.only_playback { "ACTIVO" } else { "INACTIVO" }
                    ),
                    format!(
                        "Monitorear auriculares Cable / USB         : [{}]",
                        if draft_settings.track_wired_usb { "ACTIVO" } else { "INACTIVO" }
                    ),
                    format!(
                        "Servicio automático (systemd --user)       : [{}]",
                            if draft_systemd { "ACTIVO" } else { "INACTIVO" }
                    ),
                    "──────────────────────────────────────────────────────────────".to_string(),
                      "💾 [ GUARDAR CAMBIOS ]".to_string(),
                      "↩  [ REVERTIR CAMBIOS NO GUARDADOS ]".to_string(),
                      "🔄 [ RESTAURAR AJUSTES POR DEFECTO ]".to_string(),
                      "🛠  [ EXPORTAR REPORTE DE DEPURACIÓN (DEBUG LOG) ]".to_string(),
                ];

                let mut list_items = Vec::new();
                list_items.push(ListItem::new(Line::from(Span::styled(
                    unsaved_banner,
                    Style::default()
                    .fg(if has_unsaved { Color::Yellow } else { Color::Green })
                    .add_modifier(Modifier::BOLD),
                ))));
                list_items.push(ListItem::new(""));

                for (i, r) in rows.iter().enumerate() {
                    let is_sel = i == settings_cursor;
                    let style = if is_sel {
                        Style::default()
                        .fg(Color::Black)
                        .bg(Color::Cyan)
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
                    .title(" Ajustes de la Aplicación "),
                );
                f.render_widget(settings_list, chunks[1]);
            }

            let footer = Paragraph::new(format!(
                " {} | [Tab] Pestaña | [↑/↓] Navegar | [Enter/Espacio] Acción | [q] Salir",
                status_msg
            ))
            .style(Style::default().fg(Color::Gray))
            .block(Block::default().borders(Borders::ALL));
            f.render_widget(footer, chunks[2]);

            // Ventana emergente (Modal) en TUI con Gráfica Braille
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
                    .title(" 📊 Detalles de Sesión y Curva de Descarga (Pulsa [Esc] o [Enter] para cerrar) ")
                    .style(Style::default().bg(Color::Black).fg(Color::White));
                    f.render_widget(outer_block, area);

                    let real_model = if dev.real_name.is_empty() {
                        &dev.name
                    } else {
                        &dev.real_name
                    };
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
                    let v_inc = if sess.volume_increased {
                        "Sí (subió durante la sesión ↑)"
                    } else {
                        "No (estable)"
                    };
                    let bat_txt = match (sess.battery_start, sess.battery_end) {
                        (Some(b1), Some(b2)) => format!("{}% ➔ {}%", b1, b2),
                      _ => "Alimentación por cable (Sin batería)".into(),
                    };

                    let info_text = vec![
                        Line::from(format!(
                            " 🎧 Alias: {}   |   Modelo (Firmware): {}   |   Tipo: {}",
                                           dev.name, real_model, dev.conn_type
                        )),
                      Line::from(format!(
                          " 📡 Dirección / ID: {}   |   Códec: {}",
                          mac,
                          sess.codec.as_deref().unwrap_or("PCM/SBC")
                      )),
                      Line::from(format!(
                          " 🔊 Volumen: Inicial: {}  |  Promedio: {}  |  Pico Máx: {}  |  ¿Aumento?: {}",
                          v_start, v_avg, v_max, v_inc
                      )),
                      Line::from(format!(
                          " 🔋 Conectado: {}  |  Reproduciendo: {}  |  Batería: {}",
                          sess.connected_human, sess.listening_human, bat_txt
                      )),
                    ];
                    let info_para = Paragraph::new(info_text).block(
                        Block::default()
                        .borders(Borders::ALL)
                        .title(" Identificación y Telemetría "),
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
                        .title(" Gráfica: Batería % (Verde) vs Volumen % (Cian) | Eje Y: 0%-100% "),
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
                                         color: Color::Green,
                                });
                            }
                            if let (Some(v1), Some(v2)) = (w[0].volume, w[1].volume) {
                                ctx.draw(&CanvasLine {
                                    x1: w[0].t_sec as f64,
                                    y1: v1.min(100) as f64,
                                         x2: w[1].t_sec as f64,
                                         y2: v2.min(100) as f64,
                                         color: Color::Cyan,
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
                    KeyCode::Tab => {
                        active_tab = (active_tab + 1) % 2;
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        if active_tab == 0 {
                            selected_session_idx = selected_session_idx.saturating_sub(1);
                        } else {
                            settings_cursor = settings_cursor.saturating_sub(1);
                            if settings_cursor == 6 {
                                settings_cursor = 5;
                            }
                        }
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        if active_tab == 0 {
                            if !flat_sessions.is_empty() {
                                selected_session_idx =
                                (selected_session_idx + 1).min(flat_sessions.len() - 1);
                            }
                        } else {
                            settings_cursor = (settings_cursor + 1).min(10);
                            if settings_cursor == 6 {
                                settings_cursor = 7;
                            }
                        }
                    }
                    KeyCode::Left | KeyCode::Char('-') => {
                        if active_tab == 1 && settings_cursor == 0 {
                            draft_settings.poll_interval =
                            draft_settings.poll_interval.saturating_sub(1).max(1);
                        }
                    }
                    KeyCode::Right | KeyCode::Char('+') | KeyCode::Char('=') => {
                        if active_tab == 1 && settings_cursor == 0 {
                            draft_settings.poll_interval =
                            (draft_settings.poll_interval + 1).min(60);
                        }
                    }
                    KeyCode::Enter | KeyCode::Char(' ') => {
                        if active_tab == 0 {
                            if !flat_sessions.is_empty() {
                                show_modal = true;
                            }
                        } else {
                            match settings_cursor {
                                0 => {
                                    draft_settings.poll_interval =
                                    (draft_settings.poll_interval % 60) + 1
                                }
                                1 => draft_settings.show_seconds = !draft_settings.show_seconds,
                                2 => draft_settings.show_codec = !draft_settings.show_codec,
                                3 => draft_settings.only_playback = !draft_settings.only_playback,
                                4 => {
                                    draft_settings.track_wired_usb = !draft_settings.track_wired_usb
                                }
                                5 => draft_systemd = !draft_systemd,
                                7 => {
                                    // Guardar cambios
                                    tracker.db.settings = draft_settings.clone();
                                    tracker.save_db();
                                    set_systemd_service(draft_systemd);
                                    tracker.tick(false);
                                    status_msg = "✔ Ajustes guardados correctamente.".into();
                                }
                                8 => {
                                    // Revertir cambios
                                    draft_settings = tracker.db.settings.clone();
                                    draft_systemd = is_systemd_enabled();
                                    status_msg =
                                    "↩ Cambios revertidos al último estado guardado.".into();
                                }
                                9 => {
                                    // Restaurar por defecto
                                    draft_settings = Settings::default();
                                    tracker.db.settings = draft_settings.clone();
                                    tracker.save_db();
                                    status_msg =
                                    "🔄 Ajustes restaurados a los valores por defecto.".into();
                                }
                                10 => {
                                    let out_file = PathBuf::from(format!(
                                        "ylih_debug_{}.log",
                                        Local::now().format("%Y%m%d_%H%M%S")
                                    ));
                                    if write_debug_log(&out_file).is_ok() {
                                        status_msg = format!(
                                            "🛠 Log exportado en ./{}",
                                            out_file.display()
                                        );
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

// ==================== VENTANA EMERGENTE DE GRÁFICA Y DETALLES (GTK3) ====================
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
    dlg.set_default_size(580, 450);
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
                "<b>Alias:</b> {}   |   <b>Modelo (Firmware):</b> {}   |   <b>Tipo:</b> {}",
                     glib::markup_escape_text(&dev.name),
                     glib::markup_escape_text(real_model),
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
                "<b>Dirección / ID:</b> <tt>{}</tt>   |   <b>Códec / Transporte:</b> {}",
                glib::markup_escape_text(mac),
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

    let bat_summary = match (sess.battery_start, sess.battery_end) {
        (Some(b1), Some(b2)) => format!("{}% ➔ {}%", b1, b2),
        _ => "Alimentación por cable (Sin batería)".into(),
    };

    info_box.pack_start(
        &icon_label_row(
            "battery-good-symbolic",
            &format!(
                "<b>Tiempo Conectado:</b> {}   |   <b>Reproduciendo:</b> {}   |   <b>Batería:</b> {}",
                sess.connected_human, sess.listening_human, bat_summary
            ),
        ),
        false,
        false,
        0,
    );

    area.pack_start(&info_frame, false, false, 0);

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

// ==================== MODO GTK3 NATIVO CON GUARDAR / REVERTIR / DEFAULT ====================
fn run_gtk(tracker: Tracker) {
    gtk::init().expect("Error inicializando GTK3");
    let tracker = Rc::new(RefCell::new(tracker));

    let win = gtk::Window::new(gtk::WindowType::Toplevel);
    win.set_title("YLIH - Your Life in Headphones");
    win.set_default_size(660, 560);
    win.set_border_width(10);
    win.set_icon_name(Some("audio-headphones"));

    let notebook = gtk::Notebook::new();
    win.add(&notebook);

    // Pestaña 1: Dispositivos
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

    // Pestaña 2: Ajustes
    let tab_settings = gtk::Box::new(gtk::Orientation::Vertical, 12);
    tab_settings.set_border_width(16);
    let tab2_hdr = icon_label_row("preferences-system-symbolic", "Ajustes");
    tab2_hdr.show_all();
    notebook.append_page(&tab_settings, Some(&tab2_hdr));

    let grid = gtk::Grid::new();
    grid.set_column_spacing(20);
    grid.set_row_spacing(12);
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
        "<b>Mostrar códec / transporte (AAC/SBC/PCM):</b>",
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

    let row_wired = icon_label_row(
        "audio-headphones-symbolic",
        "<b>Monitorear también auriculares por Cable / USB:</b>",
    );
    let sw_wired = gtk::Switch::new();
    sw_wired.set_active(s.track_wired_usb);
    sw_wired.set_halign(gtk::Align::Start);
    grid.attach(&row_wired, 0, 4, 1, 1);
    grid.attach(&sw_wired, 1, 4, 1, 1);

    let row_sysd = icon_label_row(
        "system-run-symbolic",
        "<b>Servicio automático (systemd --user):</b>",
    );
    let sw_sysd = gtk::Switch::new();
    sw_sysd.set_active(is_systemd_enabled());
    sw_sysd.set_halign(gtk::Align::Start);
    grid.attach(&row_sysd, 0, 5, 1, 1);
    grid.attach(&sw_sysd, 1, 5, 1, 1);

    // Barra de botones: Guardar, Revertir y Restaurar por defecto
    let action_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let btn_save = gtk::Button::new();
    btn_save.add(&icon_label_row("document-save-symbolic", "<b>Guardar cambios</b>"));

    let btn_revert = gtk::Button::new();
    btn_revert.add(&icon_label_row("edit-undo-symbolic", "Revertir"));

    let btn_default = gtk::Button::new();
    btn_default.add(&icon_label_row("view-refresh-symbolic", "Por defecto"));

    action_box.pack_start(&btn_save, true, true, 0);
    action_box.pack_start(&btn_revert, true, true, 0);
    action_box.pack_start(&btn_default, true, true, 0);
    tab_settings.pack_start(&action_box, false, false, 6);

    let lbl_settings_status = gtk::Label::new(None);
    lbl_settings_status.set_markup("<small><i>Modifica tus preferencias y pulsa «Guardar cambios».</i></small>");
    lbl_settings_status.set_xalign(0.0);
    tab_settings.pack_start(&lbl_settings_status, false, false, 0);

    let sep = gtk::Separator::new(gtk::Orientation::Horizontal);
    tab_settings.pack_start(&sep, false, false, 4);

    let btn_import = gtk::Button::new();
    btn_import.add(&icon_label_row(
        "document-open-symbolic",
        "Importar y fusionar log (.json)",
    ));
    tab_settings.pack_start(&btn_import, false, false, 2);

    let btn_debug = gtk::Button::new();
    btn_debug.add(&icon_label_row(
        "utilities-terminal-symbolic",
        "Exportar paquete de depuración (Debug Log)",
    ));
    tab_settings.pack_start(&btn_debug, false, false, 2);

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
                    "<span foreground='#4ade80'><b>CONECTADO</b></span>"
                } else {
                    "<span foreground='#9ca3af'><b>DESCONECTADO</b></span>"
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
                    } else {
                        card.pack_start(
                            &icon_label_row(
                                "ac-adapter-symbolic",
                                "<b>Alimentación:</b>  Por cable / Sin batería reportada",
                            ),
                            false,
                            false,
                            2,
                        );
                    }

                    for sess in dev.sessions.iter().rev().take(6) {
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
                                "{}  |  Conectado: {}  |  Sonando: {}{}{}  [{}]",
                                &sess.start.replace('T', " "),
                                     sess.connected_human,
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

    // Acción 1: Guardar cambios
    {
        let tracker = tracker.clone();
        let refresh = refresh_ui.clone();
        let spin = spin.clone();
        let sw_sec = sw_sec.clone();
        let sw_codec = sw_codec.clone();
        let sw_only = sw_only.clone();
        let sw_wired = sw_wired.clone();
        let sw_sysd = sw_sysd.clone();
        let lbl_status = lbl_settings_status.clone();

        btn_save.connect_clicked(move |_| {
            {
                let mut t = tracker.borrow_mut();
                t.db.settings.poll_interval = spin.value_as_int() as u64;
                t.db.settings.show_seconds = sw_sec.is_active();
                t.db.settings.show_codec = sw_codec.is_active();
                t.db.settings.only_playback = sw_only.is_active();
                t.db.settings.track_wired_usb = sw_wired.is_active();
                t.save_db();
                t.tick(false);
            }
            set_systemd_service(sw_sysd.is_active());
            refresh();
            lbl_status.set_markup(
                "<span foreground='#4ade80'><b>✔ Ajustes guardados y aplicados correctamente.</b></span>",
            );
        });
    }

    // Acción 2: Revertir cambios no guardados
    {
        let tracker = tracker.clone();
        let spin = spin.clone();
        let sw_sec = sw_sec.clone();
        let sw_codec = sw_codec.clone();
        let sw_only = sw_only.clone();
        let sw_wired = sw_wired.clone();
        let sw_sysd = sw_sysd.clone();
        let lbl_status = lbl_settings_status.clone();

        btn_revert.connect_clicked(move |_| {
            let s = tracker.borrow().db.settings.clone();
            spin.set_value(s.poll_interval as f64);
            sw_sec.set_active(s.show_seconds);
            sw_codec.set_active(s.show_codec);
            sw_only.set_active(s.only_playback);
            sw_wired.set_active(s.track_wired_usb);
            sw_sysd.set_active(is_systemd_enabled());
            lbl_status.set_markup(
                "<span foreground='#60a5fa'><b>↩ Cambios descartados (revertido al último estado guardado).</b></span>",
            );
        });
    }

    // Acción 3: Restaurar por defecto
    {
        let tracker = tracker.clone();
        let refresh = refresh_ui.clone();
        let spin = spin.clone();
        let sw_sec = sw_sec.clone();
        let sw_codec = sw_codec.clone();
        let sw_only = sw_only.clone();
        let sw_wired = sw_wired.clone();
        let lbl_status = lbl_settings_status.clone();

        btn_default.connect_clicked(move |_| {
            let def = Settings::default();
            spin.set_value(def.poll_interval as f64);
            sw_sec.set_active(def.show_seconds);
            sw_codec.set_active(def.show_codec);
            sw_only.set_active(def.only_playback);
            sw_wired.set_active(def.track_wired_usb);
            {
                let mut t = tracker.borrow_mut();
                t.db.settings = def;
                t.save_db();
                t.tick(false);
            }
            refresh();
            lbl_status.set_markup(
                "<span foreground='#facc15'><b>🔄 Ajustes restaurados a los valores por defecto.</b></span>",
            );
        });
    }

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
