# 🎧 YLIH-RS (Your Life in Headphones - Linux)

Monitor ultraligero en Rust (GTK3 + TUI + Systemd Daemon) inspirado en `ylih` de F-Droid para registrar cuántas horas de vida útil real y reproducción te dan tus auriculares Bluetooth en Linux (BlueZ + PipeWire).
## Capturas de pantalla del programa


Interfaz en TUI:
<img width="1040" height="627" alt="2026-10-22_10-36-19" src="https://github.com/user-attachments/assets/8a5c229c-de35-4e44-b051-85103b46d662" />
<img width="1040" height="626" alt="2026-10-22_10-36-35" src="https://github.com/user-attachments/assets/6920617c-fe38-44cd-8010-148672288645" />
<img width="1044" height="628" alt="2026-10-22_10-36-54" src="https://github.com/user-attachments/assets/dfcb5c7d-8918-47fb-a1bc-1cdc202d4926" />

Interfaz en GTK3:
<img width="901" height="649" alt="2026-10-22_10-37-54" src="https://github.com/user-attachments/assets/1815858b-a4bb-4f6f-83e1-9459ba7c5018" />
<img width="872" height="613" alt="2026-10-22_10-38-07" src="https://github.com/user-attachments/assets/4a5864f3-094b-4a26-baae-5c2e39b42487" />
<img width="901" height="649" alt="2026-10-22_10-38-47" src="https://github.com/user-attachments/assets/8d8c88ed-c6fc-4b77-aba6-896ebc31fa3a" />
Todas estas capturas provienen de la versión 2.1.1 

## Modos de uso
* `ylih` : Abre la interfaz gráfica en **GTK3**.
* `ylih --tui` : Abre la interfaz en terminal (**Ratatui**).
* `ylih --daemon` : Ejecuta el demonio silencioso en segundo plano.
## ¿Cómo compilarlo e instalarlo?
1. Dependencias de Compilación (Debian / Ubuntu / Mint)
Para compilar el proyecto desde el código fuente, asegúrate de tener instaladas las herramientas de compilación de Rust y las librerías de desarrollo de GTK3 y D-Bus:

```bash
sudo apt update
sudo apt install build-essential curl git pkg-config libgtk-3-dev libglib2.0-dev libdbus-1-dev
```
2. Clonar el repositorio GitHub:
```bash
git clone https://github.com/SnowyAero/ylih-rs.git
cd ylih-rs
```
3. Compilar el binario:
```bash
cargo build --release -j 4
```
(Nota: si tienes un procesador de cuatro núcleos o más, puedes cambiar el flag "-j4" por la cantidad de núcleos que tengas tu CPU o puedes quitarlo si quieres)

4. Instalar el binario compilado a nivel de usuario:
```bash
mkdir -p ~/.local/bin ~/.local/share/applications

# Copiar binario ejecutable
cp target/release/ylih ~/.local/bin/ylih
chmod +x ~/.local/bin/ylih

# Copiar archivo .desktop para el menú de aplicaciones
cp ylih.desktop ~/.local/share/applications/ylih.desktop

echo "✔ ¡YLIH instalado con éxito en ~/.local/bin/ylih!"
```
## Licencia
Distribuido bajo la licencia **GNU GPLv3** (`GPL-3.0-or-later`).
