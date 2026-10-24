// SPDX-License-Identifier: GPL-3.0-or-later
use std::{
    collections::{BTreeMap, HashMap},
    env, fs,
    path::PathBuf,
    process::Command,
};
use zbus::{blocking::Connection, zvariant::OwnedValue};

const AUDIO_UUIDS: &[&str] = &[
    "0000110b-0000-1000-8000-00805f9b34fb",
    "00001108-0000-1000-8000-00805f9b34fb",
    "0000111e-0000-1000-8000-00805f9b34fb",
    "0000110d-0000-1000-8000-00805f9b34fb",
];

#[derive(Clone, Debug)]
pub struct DetectedAudioDevice {
    pub alias: String,
    pub real_name: String,
    pub conn_type: String,
    pub connected: bool,
    pub battery: Option<u8>,
    pub codec: Option<String>,
}

pub fn format_short(sec: u64) -> String {
    let h = sec / 3600;
    let m = (sec % 3600) / 60;
    let s = sec % 60;
    if h > 0 {
        format!("{}h {:02}m", h, m)
    } else {
        format!("{}m {:02}s", m, s)
    }
}

pub fn format_time(sec: u64, show_sec: bool) -> String {
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

pub fn get_current_volume() -> Option<u8> {
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

fn parse_u8_from_zvariant(val: &OwnedValue) -> Option<u8> {
    if let Ok(v) = u8::try_from(val) {
        return Some(v.min(100));
    }
    if let Ok(v) = zbus::zvariant::Value::try_from(val.clone()) {
        match v {
            zbus::zvariant::Value::U8(x) => return Some(x.min(100)),
            zbus::zvariant::Value::U16(x) => return Some((x as u8).min(100)),
            zbus::zvariant::Value::U32(x) => return Some((x as u8).min(100)),
            zbus::zvariant::Value::I32(x) => return Some((x.max(0) as u8).min(100)),
            _ => {}
        }
    }
    None
}

fn query_bt_battery_fallback(mac: &str) -> Option<u8> {
    let mac_low = mac.to_lowercase();
    let mac_dash = mac_low.replace(':', "-");
    let mac_under = mac_low.replace(':', "_");
    if let Ok(entries) = fs::read_dir("/sys/class/power_supply") {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_lowercase();
            let path = entry.path();
            let uevent = fs::read_to_string(path.join("uevent")).unwrap_or_default().to_lowercase();
            if name.contains(&mac_low)
                || name.contains(&mac_dash)
                || name.contains(&mac_under)
                || uevent.contains(&mac_low)
                || uevent.contains(&mac_dash)
            {
                if let Ok(cap_str) = fs::read_to_string(path.join("capacity")) {
                    if let Ok(pct) = cap_str.trim().parse::<u8>() {
                        return Some(pct.min(100));
                    }
                }
            }
        }
    }

    if let Ok(out) = Command::new("bluetoothctl").args(["info", mac]).output() {
        let s = String::from_utf8_lossy(&out.stdout);
        for line in s.lines() {
            if line.contains("Battery Percentage") {
                if let (Some(l), Some(r)) = (line.find('('), line.find(')')) {
                    if let Ok(pct) = line[l + 1..r].trim().parse::<u8>() {
                        return Some(pct.min(100));
                    }
                }
            }
        }
    }

    if let Ok(out) = Command::new("upower").arg("-d").output() {
        let s = String::from_utf8_lossy(&out.stdout);
        for block in s.split("Device:") {
            let b_low = block.to_lowercase();
            if b_low.contains(&mac_low) || b_low.contains(&mac_dash) || b_low.contains(&mac_under) {
                for line in block.lines() {
                    let l = line.trim();
                    if l.starts_with("percentage:") {
                        let num = l.trim_start_matches("percentage:").trim().trim_end_matches('%');
                        if let Ok(pct) = num.parse::<f32>() {
                            return Some(pct.round().clamp(0.0, 100.0) as u8);
                        }
                    }
                }
            }
        }
    }
    None
}

pub fn query_usb_headset_battery() -> Option<u8> {
    if let Ok(entries) = fs::read_dir("/sys/class/power_supply") {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_lowercase();
            if name.starts_with("bat")
                || name.starts_with("ac")
                || name.starts_with("adp")
                || name.starts_with("ucsi")
            {
                continue;
            }
            let path = entry.path();
            let uevent = fs::read_to_string(path.join("uevent")).unwrap_or_default().to_lowercase();
            if name.contains("bluez") || uevent.contains("bluetooth") {
                continue;
            }
            let dev_type = fs::read_to_string(path.join("type")).unwrap_or_default();
            let scope = fs::read_to_string(path.join("scope")).unwrap_or_default();
            if dev_type.trim().eq_ignore_ascii_case("Battery")
                || scope.trim().eq_ignore_ascii_case("Device")
            {
                if let Ok(cap_str) = fs::read_to_string(path.join("capacity")) {
                    if let Ok(pct) = cap_str.trim().parse::<u8>() {
                        return Some(pct.min(100));
                    }
                }
            }
        }
    }
    None
}

pub fn inspect_pipewire(
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
            let dev_api = props
                .get("device.api")
                .and_then(|v| v.as_str())
                .unwrap_or("");

            if media_class == "Audio/Sink" && dev_api != "bluez5" {
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
                    .unwrap_or("Auriculares USB");
                let product = props
                    .get("device.product.name")
                    .or_else(|| props.get("alsa.card_name"))
                    .and_then(|v| v.as_str())
                    .unwrap_or(desc);

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

        if track_wired_usb {
            let dev_api = obj
                .pointer("/info/props/device.api")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let dev_bus = obj
                .pointer("/info/props/device.bus")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let is_bt = obj.pointer("/info/props/api.bluez5.address").is_some();

            if dev_api == "alsa" && !dev_bus.eq_ignore_ascii_case("usb") && !is_bt {
                let card_name = obj
                    .pointer("/info/props/alsa.card_name")
                    .or_else(|| obj.pointer("/info/props/device.description"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("Audio Interno");

                if let Some(r_list) = obj.pointer("/info/params/Route").and_then(|v| v.as_array()) {
                    for r in r_list {
                        let r_name = r
                            .get("name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_lowercase();
                        let avail = r.get("available").and_then(|v| v.as_str()).unwrap_or("");

                        if (r_name.contains("headphone") || r_name.contains("headset"))
                            && avail == "yes"
                        {
                            wired_devs.insert(
                                "JACK:3.5MM".into(),
                                DetectedAudioDevice {
                                    alias: format!("Auriculares ({})", card_name),
                                    real_name: card_name.to_string(),
                                    conn_type: "Jack 3.5mm".into(),
                                    connected: true,
                                    battery: None,
                                    codec: Some("Analógico 3.5mm".into()),
                                },
                            );
                        }
                    }
                }
            }
        }
    }

    (bt_codecs, wired_devs)
}

pub fn query_all_audio_devices(track_wired_usb: bool) -> BTreeMap<String, DetectedAudioDevice> {
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
            .and_then(parse_u8_from_zvariant)
            .or_else(|| {
                if connected {
                    query_bt_battery_fallback(&mac)
                } else {
                    None
                }
            });

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

pub fn is_audio_playing() -> bool {
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

pub fn service_path() -> PathBuf {
    let home = env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".config/systemd/user/ylih.service")
}

pub fn is_systemd_enabled() -> bool {
    Command::new("systemctl")
        .args(["--user", "is-active", "ylih.service"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "active")
        .unwrap_or(false)
}

pub fn set_systemd_service(enable: bool) {
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
