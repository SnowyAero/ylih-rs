// SPDX-License-Identifier: GPL-3.0-or-later
use crate::{
    config::Settings,
    i18n::I18n,
    telemetry::{
        format_short, get_current_volume, is_audio_playing, query_all_audio_devices,
    },
};
use chrono::Local;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashSet},
    env, fs,
    path::PathBuf,
    process::Command,
    time::Instant,
};

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct TelemetryPoint {
    pub t_sec: u64,
    pub battery: Option<u8>,
    pub volume: Option<u8>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Session {
    pub start: String,
    pub end: String,
    pub connected_seconds: u64,
    pub listening_seconds: u64,
    pub connected_human: String,
    pub listening_human: String,
    pub battery_start: Option<u8>,
    pub battery_end: Option<u8>,
    #[serde(default)]
    pub codec: Option<String>,
    pub status: String,
    #[serde(default)]
    pub volume_start: Option<u8>,
    #[serde(default)]
    pub volume_avg: Option<u8>,
    #[serde(default)]
    pub volume_max: Option<u8>,
    #[serde(default)]
    pub volume_increased: bool,
    #[serde(default)]
    pub samples: Vec<TelemetryPoint>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct DeviceRecord {
    #[serde(default)]
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub real_name: String,
    #[serde(default = "default_conn_type")]
    pub conn_type: String,
    #[serde(default)]
    pub custom_image: Option<String>,
    pub lifetime_connected_seconds: u64,
    pub lifetime_listening_seconds: u64,
    pub lifetime_connected_human: String,
    pub lifetime_listening_human: String,
    pub sessions: Vec<Session>,
    #[serde(default)]
    pub currently_connected: bool,
    #[serde(default)]
    pub current_battery: Option<u8>,
    #[serde(default)]
    pub current_codec: Option<String>,
    #[serde(default)]
    pub current_volume: Option<u8>,
}

fn default_conn_type() -> String {
    "Bluetooth".into()
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Database {
    #[serde(default)]
    pub settings: Settings,
    #[serde(default)]
    pub devices: BTreeMap<String, DeviceRecord>,
}

pub fn base_data_dir() -> PathBuf {
    let home = env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".local/share/ylih")
}

pub fn applogs_dir_path() -> PathBuf {
    let home = env::var("HOME").unwrap_or_else(|_| ".".into());
    let p = PathBuf::from(home).join(".config/ylih-rs/applogs");
    let _ = fs::create_dir_all(&p);
    p
}

pub fn config_file_path() -> PathBuf {
    base_data_dir().join("config.json")
}

pub fn devices_dir_path() -> PathBuf {
    base_data_dir().join("devices")
}

pub fn legacy_log_path() -> PathBuf {
    base_data_dir().join("ylih_log.json")
}

pub fn sanitize_id_filename(id: &str) -> String {
    let clean: String = id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    format!("{}.json", clean)
}

pub struct Tracker {
    pub db: Database,
    pub i18n: I18n,
    last_tick: Instant,
}

impl Tracker {
    pub fn new(fast_ui_start: bool) -> Self {
        let db = Self::load_modular_db();
        let i18n = I18n::new(&db.settings.language);
        let mut t = Self {
            db,
            i18n,
            last_tick: Instant::now(),
        };
        t.sanitize_db();
        if !fast_ui_start {
            t.clean_orphan_sessions();
            t.tick(true);
        }
        t
    }

    pub fn reload_i18n(&mut self) {
        self.i18n = I18n::new(&self.db.settings.language);
    }

    pub fn load_modular_db() -> Database {
        let base = base_data_dir();
        let dev_dir = devices_dir_path();
        let _ = fs::create_dir_all(&dev_dir);

        let cfg_path = config_file_path();
        let legacy_path = legacy_log_path();

        let dir_is_empty = fs::read_dir(&dev_dir)
            .map(|mut d| d.next().is_none())
            .unwrap_or(true);

        if legacy_path.exists() && dir_is_empty {
            if let Ok(content) = fs::read_to_string(&legacy_path) {
                if let Ok(mut old_db) = serde_json::from_str::<Database>(&content) {
                    for (id, dev) in old_db.devices.iter_mut() {
                        dev.id = id.clone();
                    }
                    let temp_tracker = Self {
                        i18n: I18n::new(&old_db.settings.language),
                        db: old_db,
                        last_tick: Instant::now(),
                    };
                    temp_tracker.save_db();
                    let _ = fs::rename(&legacy_path, base.join("ylih_log.json.bak"));
                    return temp_tracker.db;
                }
            }
        }

        let settings: Settings = fs::read_to_string(&cfg_path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();

        let mut devices = BTreeMap::new();
        if let Ok(entries) = fs::read_dir(&dev_dir) {
            for entry in entries.flatten() {
                let p = entry.path();
                if p.extension().and_then(|e| e.to_str()) == Some("json") {
                    if let Ok(content) = fs::read_to_string(&p) {
                        if let Ok(mut dev) = serde_json::from_str::<DeviceRecord>(&content) {
                            let key = if !dev.id.is_empty() {
                                dev.id.clone()
                            } else {
                                p.file_stem()
                                    .unwrap_or_default()
                                    .to_string_lossy()
                                    .replace('_', ":")
                            };
                            dev.id = key.clone();
                            devices.insert(key, dev);
                        }
                    }
                }
            }
        }

        Database { settings, devices }
    }

    pub fn save_db(&self) {
        let dev_dir = devices_dir_path();
        let _ = fs::create_dir_all(&dev_dir);

        let cfg_path = config_file_path();
        let cfg_tmp = cfg_path.with_extension("json.tmp");
        if let Ok(cfg_json) = serde_json::to_string_pretty(&self.db.settings) {
            if fs::write(&cfg_tmp, cfg_json).is_ok() {
                let _ = fs::rename(&cfg_tmp, &cfg_path);
            }
        }

        for (id, dev) in &self.db.devices {
            let mut record = dev.clone();
            record.id = id.clone();
            let file_path = dev_dir.join(sanitize_id_filename(id));
            let tmp_path = file_path.with_extension("json.tmp");
            if let Ok(dev_json) = serde_json::to_string_pretty(&record) {
                if fs::write(&tmp_path, dev_json).is_ok() {
                    let _ = fs::rename(&tmp_path, &file_path);
                }
            }
        }
    }

    pub fn delete_device(&mut self, id: &str) -> Result<(), String> {
        if let Some(dev) = self.db.devices.get(id) {
            if dev.currently_connected {
                return Err("Device is currently connected".into());
            }
        }
        self.db.devices.remove(id);
        let file_path = devices_dir_path().join(sanitize_id_filename(id));
        let _ = fs::remove_file(file_path);
        self.save_db();
        Ok(())
    }

    pub fn sanitize_db(&mut self) {
        let bt_names: Vec<String> = self
            .db
            .devices
            .iter()
            .filter(|(k, v)| *k != "JACK:3.5MM" && v.conn_type == "Bluetooth")
            .map(|(_, v)| v.name.clone())
            .collect();

        if let Some(jack) = self.db.devices.get_mut("JACK:3.5MM") {
            let contaminated = bt_names.iter().any(|bn| {
                !bn.is_empty() && (jack.name.contains(bn) || jack.real_name.contains(bn))
            });
            if contaminated {
                jack.name = "Auriculares (Audio Interno)".into();
                jack.real_name = "Audio Interno".into();
            }
        }

        // Corregir falso pico de volumen en t_sec=0 cuando PipeWire aún reportaba el volumen del altavoz interno
        for dev in self.db.devices.values_mut() {
            for sess in &mut dev.sessions {
                if sess.samples.len() >= 2 {
                    let p0_t = sess.samples[0].t_sec;
                    let p1_t = sess.samples[1].t_sec;
                    if p0_t == 0 && p1_t <= 12 {
                        if let (Some(v0), Some(v1)) = (sess.samples[0].volume, sess.samples[1].volume) {
                            let bat_delayed = sess.samples[0].battery.is_none() && sess.samples[1].battery.is_some();
                            let handshake_jump = v1.abs_diff(v0) >= 10;
                            if bat_delayed || handshake_jump {
                                sess.samples[0].volume = Some(v1);
                                if sess.samples[0].battery.is_none() {
                                    sess.samples[0].battery = sess.samples[1].battery;
                                }
                                sess.volume_start = Some(v1);
                            }
                        }
                    }
                    if let Some(v_init) = sess.volume_start {
                        let max_v = sess
                            .samples
                            .iter()
                            .filter_map(|p| p.volume)
                            .max()
                            .unwrap_or(v_init);
                        let vols: Vec<u64> = sess
                            .samples
                            .iter()
                            .filter_map(|p| p.volume.map(|v| v as u64))
                            .collect();
                        if !vols.is_empty() {
                            sess.volume_avg = Some((vols.iter().sum::<u64>() / vols.len() as u64) as u8);
                        }
                        sess.volume_max = Some(max_v);
                        // Solo marcar incremento si el usuario subió el volumen después de los primeros 12s de handshake
                        sess.volume_increased = sess
                            .samples
                            .iter()
                            .any(|p| p.t_sec > 12 && p.volume.map(|v| v > v_init).unwrap_or(false));
                    }
                }
            }
        }
    }

    pub fn clean_orphan_sessions(&mut self) {
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

    pub fn update_totals(dev: &mut DeviceRecord) {
        let total_conn: u64 = dev.sessions.iter().map(|s| s.connected_seconds).sum();
        let total_play: u64 = dev.sessions.iter().map(|s| s.listening_seconds).sum();
        dev.lifetime_connected_seconds = total_conn;
        dev.lifetime_listening_seconds = total_play;
        dev.lifetime_connected_human = format_short(total_conn);
        dev.lifetime_listening_human = format_short(total_play);
    }

    pub fn import_external_db(&mut self, ext_path: &str) -> Result<usize, String> {
        let content = fs::read_to_string(ext_path).map_err(|e| e.to_string())?;
        let mut imported = 0;

        if let Ok(single_dev) = serde_json::from_str::<DeviceRecord>(&content) {
            if !single_dev.id.is_empty() || !single_dev.sessions.is_empty() {
                let id = if single_dev.id.is_empty() {
                    single_dev.name.to_uppercase()
                } else {
                    single_dev.id.to_uppercase()
                };
                imported += self.merge_device(id, single_dev);
                self.sanitize_db();
                self.save_db();
                return Ok(imported);
            }
        }

        let ext: Database = serde_json::from_str(&content).map_err(|e| e.to_string())?;
        for (mac, edev) in ext.devices {
            imported += self.merge_device(mac.to_uppercase(), edev);
        }
        self.sanitize_db();
        self.save_db();
        Ok(imported)
    }

    fn merge_device(&mut self, id: String, mut edev: DeviceRecord) -> usize {
        edev.id = id.clone();
        let mut count = 0;
        if let Some(local_dev) = self.db.devices.get_mut(&id) {
            let existing: HashSet<String> =
                local_dev.sessions.iter().map(|s| s.start.clone()).collect();
            for s in edev.sessions {
                if !existing.contains(&s.start) {
                    local_dev.sessions.push(s);
                    count += 1;
                }
            }
            local_dev.sessions.sort_by(|a, b| a.start.cmp(&b.start));
            Self::update_totals(local_dev);
        } else {
            count += edev.sessions.len();
            self.db.devices.insert(id, edev);
        }
        count
    }

    pub fn tick(&mut self, initial: bool) {
        if let Ok(cfg_str) = fs::read_to_string(config_file_path()) {
            if let Ok(latest_cfg) = serde_json::from_str::<Settings>(&cfg_str) {
                if latest_cfg.language != self.db.settings.language {
                    self.i18n = I18n::new(&latest_cfg.language);
                }
                self.db.settings = latest_cfg;
            }
        }

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
            let dev = self
                .db
                .devices
                .entry(id.clone())
                .or_insert_with(|| DeviceRecord {
                    id: id.clone(),
                    name: info.alias.clone(),
                    real_name: info.real_name.clone(),
                    conn_type: info.conn_type.clone(),
                    custom_image: None,
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

            dev.id = id;
            dev.name = info.alias;
            if !info.real_name.is_empty() {
                dev.real_name = info.real_name;
            }
            dev.conn_type = info.conn_type;
            dev.currently_connected = info.connected;
            if info.battery.is_some() {
                dev.current_battery = info.battery;
            } else if dev.current_battery.is_none() {
                dev.current_battery = dev
                    .sessions
                    .iter()
                    .rev()
                    .find_map(|s| s.battery_end.or(s.battery_start));
            }
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

                    // Ventana de estabilización de PipeWire (primeros 12s):
                    // Si el volumen inicial era del altavoz interno y salta al nivel real del casco Bluetooth, recalibrar base.
                    if let Some(vol) = current_vol {
                        if active.connected_seconds <= 12 {
                            let jump = active
                                .volume_start
                                .map(|vs| vol.abs_diff(vs) >= 8)
                                .unwrap_or(true);
                            if jump || active.samples.len() <= 2 {
                                active.volume_start = Some(vol);
                                active.volume_max = Some(vol);
                                active.volume_increased = false;
                                for sp in active.samples.iter_mut() {
                                    sp.volume = Some(vol);
                                    if sp.battery.is_none() {
                                        sp.battery = info.battery;
                                    }
                                }
                            }
                        } else {
                            let v_start = *active.volume_start.get_or_insert(vol);
                            if vol > v_start {
                                active.volume_increased = true;
                            }
                            let v_max = active.volume_max.unwrap_or(vol).max(vol);
                            active.volume_max = Some(v_max);
                        }
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
        self.sanitize_db();
        self.save_db();
    }
}

pub fn write_debug_log(path: &PathBuf) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let mut report = String::new();
    report.push_str("=== YLIH-RS v2.2.0-preview2 DEBUG REPORT ===\n");
    report.push_str(&format!("Fecha: {}\n\n", Local::now()));

    let db_tmp = Tracker::load_modular_db();
    if let Ok(json_content) = serde_json::to_string_pretty(&db_tmp) {
        report.push_str("--- YLIH MODULAR JSON DATABASE ---\n");
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

    report.push_str("--- BLUETOOTHCTL INFO ---\n");
    for (id, dev) in &db_tmp.devices {
        if dev.conn_type == "Bluetooth" {
            if let Ok(out) = Command::new("bluetoothctl").args(["info", id]).output() {
                report.push_str(&String::from_utf8_lossy(&out.stdout));
                report.push_str("\n");
            }
        }
    }

    if let Ok(out) = Command::new("upower").arg("-d").output() {
        report.push_str("\n--- UPOWER STATUS ---\n");
        report.push_str(&String::from_utf8_lossy(&out.stdout));
    }

    fs::write(path, report)
}
