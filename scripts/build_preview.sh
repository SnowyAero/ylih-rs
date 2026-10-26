#!/bin/bash
set -e

VERSION="2.2.0-preview2"
echo "🚀 [1/5] Compilando YLIH v${VERSION} en modo release..."
cargo build --release -j 2

echo "📦 [2/5] Preparando estructura en dist/..."
rm -rf dist/
mkdir -p dist/deb/DEBIAN
mkdir -p dist/deb/usr/bin
mkdir -p dist/deb/usr/share/applications
mkdir -p dist/deb/usr/share/ylih/locales
mkdir -p dist/deb/usr/share/doc/ylih
mkdir -p "dist/ylih-${VERSION}-linux-x86_64/locales"
mkdir -p "dist/ylih-${VERSION}-linux-x86_64/docs"

# Copiar archivos para el paquete .deb
cp target/release/ylih dist/deb/usr/bin/ylih
chmod 755 dist/deb/usr/bin/ylih
cp ylih.desktop dist/deb/usr/share/applications/
cp locales/*.pak dist/deb/usr/share/ylih/locales/
cp LICENSE README.md dist/deb/usr/share/doc/ylih/ 2>/dev/null || true
cp docs/*.md dist/deb/usr/share/doc/ylih/ 2>/dev/null || true

cat << CONTROL > dist/deb/DEBIAN/control
Package: ylih
Version: ${VERSION}
Section: sound
Priority: optional
Architecture: amd64
Depends: libgtk-3-0, libglib2.0-0, bluez, playerctl, pipewire, wireplumber
Maintainer: SnowyAero <snowylogs@debian>
Description: Your Life in Headphones (Preview v2.2.0)
 Monitor de auriculares Bluetooth, USB y Jack 3.5mm en Rust.
 Incluye arquitectura modular, sistema de idiomas (.pak), bases de datos JSON
 independientes por dispositivo y personalización de gráficas (Líneas, Barras, Torta).
CONTROL

echo "🧱 [3/5] Construyendo paquete Debian (.deb)..."
dpkg-deb --build --root-owner-group dist/deb "dist/ylih_${VERSION}_amd64.deb"

echo "🗜️ [4/5] Construyendo binario portable (.tar.gz)..."
cp target/release/ylih ylih.desktop "dist/ylih-${VERSION}-linux-x86_64/"
cp LICENSE README.md "dist/ylih-${VERSION}-linux-x86_64/" 2>/dev/null || true
cp locales/*.pak "dist/ylih-${VERSION}-linux-x86_64/locales/"
cp docs/*.md "dist/ylih-${VERSION}-linux-x86_64/docs/" 2>/dev/null || true

cat << 'INSTALL' > "dist/ylih-${VERSION}-linux-x86_64/install.sh"
#!/bin/sh
set -e
mkdir -p ~/.local/bin ~/.local/share/applications ~/.local/share/ylih/locales
cp ylih ~/.local/bin/ylih
chmod +x ~/.local/bin/ylih
cp ylih.desktop ~/.local/share/applications/ylih.desktop
cp locales/*.pak ~/.local/share/ylih/locales/
echo "✔ YLIH v2.2.0-preview2 instalado correctamente en ~/.local/bin/ylih"
INSTALL
chmod +x "dist/ylih-${VERSION}-linux-x86_64/install.sh"

tar -czvf "dist/ylih-${VERSION}-linux-x86_64.tar.gz" -C dist "ylih-${VERSION}-linux-x86_64"

echo "📜 [5/5] Empaquetando código fuente limpio (.tar.gz)..."
git add Cargo.toml Cargo.lock src/ locales/ docs/ scripts/ 2>/dev/null || true
git archive --format=tar.gz --prefix="ylih-rs-${VERSION}-src/" -o "dist/ylih-rs-${VERSION}-src.tar.gz" HEAD || \
tar --exclude='./target' --exclude='./dist' --exclude='./.git' -czvf "dist/ylih-rs-${VERSION}-src.tar.gz" .

echo "✅ ¡Listo! Paquetes generados en ~/ylih-rs/dist/:"
ls -lh dist/*.deb dist/*.tar.gz
