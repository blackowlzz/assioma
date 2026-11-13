#!/bin/sh
# Builds the AppImage and installs it for the current user.
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$root/src-tauri"
NO_STRIP=true
export NO_STRIP
cargo tauri build --bundles appimage

image=$(ls -t target/release/bundle/appimage/*.AppImage | head -n 1)
apps=$HOME/Applications
bin=$HOME/.local/bin
share=$HOME/.local/share
mkdir -p "$apps" "$bin" "$share/applications" "$share/icons/hicolor/256x256/apps" "$root/dist"

cp "$image" "$apps/assioma.AppImage.new"
chmod +x "$apps/assioma.AppImage.new"
mv "$apps/assioma.AppImage.new" "$apps/assioma.AppImage"
cp "$image" "$root/dist/"
printf '#!/bin/sh\nnohup "%s/assioma.AppImage" "$@" >/dev/null 2>&1 </dev/null &\n' "$apps" > "$bin/assioma"
chmod +x "$bin/assioma"
cp icons/128x128@2x.png "$share/icons/hicolor/256x256/apps/assioma.png"

cat > "$share/applications/assioma.desktop" <<DESKTOP
[Desktop Entry]
Type=Application
Name=Assioma
GenericName=Code Editor
Comment=A light code editor for many languages, with a built-in AI
Exec=$apps/assioma.AppImage
Icon=assioma
Terminal=false
Categories=Development;TextEditor;IDE;
Keywords=code;editor;programming;python;javascript;rust;
StartupWMClass=assioma
DESKTOP

update-desktop-database "$share/applications" 2>/dev/null || true
gtk-update-icon-cache -q "$share/icons/hicolor" 2>/dev/null || true
echo "Installed $apps/assioma.AppImage"
