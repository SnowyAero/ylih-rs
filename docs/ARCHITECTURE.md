# Arquitectura de YLIH-RS (v2.2.0)

`ylih-rs` sigue una arquitectura modular en Rust diseñada para separar estrictamente la captura de telemetría del sistema operativo, el almacenamiento persistente por dispositivo y las interfaces de usuario (GTK3 y TUI).

## Estructura de Módulos (`src/`)
* **`main.rs`**: Punto de entrada mínimo. Enruta la ejecución hacia el demonio (`--daemon`), la interfaz de terminal (`--tui`) o la interfaz gráfica nativa (`GTK3`).
* **`config.rs`**: Define los ajustes globales (`Settings`), estilos de gráfica (`ChartType`: Líneas, Barras, Torta) y paletas de color (`ThemeColor`).
* **`i18n.rs`**: Motor de internacionalización basado en archivos `.pak` (`clave = valor`). Carga paquetes embebidos en el binario y permite sobreescritura externa en `~/.local/share/ylih/locales/`.
* **`database.rs`**: Gestiona la persistencia modular:
  * `~/.local/share/ylih/config.json`: Preferencias del usuario.
  * `~/.local/share/ylih/devices/<ID>.json`: Un archivo JSON independiente por cada auricular registrado (Bluetooth, USB o Jack 3.5mm). Incluye migración automática desde versiones `v2.1.x`.
* **`telemetry.rs`**: Sondea `pw-dump` (PipeWire/ALSA), `org.bluez` (D-Bus), `/sys/class/power_supply/`, `upower`, `wpctl` y `playerctl`.
* **`ui/`**:
  * `mod.rs`: Cálculo de cubetas históricas (7 días, 12 meses, años).
  * `gtk_app.rs`: Interfaz GTK3 con renderizado Cairo para gráficas de líneas, barras y torta.
  * `tui_app.rs`: Interfaz de terminal con Ratatui y Crossterm.
