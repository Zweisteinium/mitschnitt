#!/bin/sh
# AppImage without bundled libraries: WebKitGTK and GTK come from the system, 6 instead of 97 MB.
set -e
version=$(node -p "require('./src-tauri/tauri.conf.json').version")
bundle=$CARGO_TARGET_DIR/release/bundle
data=$bundle/deb/Mitschnitt_${version}_amd64/data/usr
dir=$bundle/appimage/Mitschnitt.AppDir
rm -rf "$dir" && mkdir -p "$dir/usr/bin"
cp "$data/bin/mitschnitt-app" "$dir/usr/bin/"
cp "$data/share/applications/Mitschnitt.desktop" "$data/share/icons/hicolor/512x512/apps/mitschnitt-app.png" "$dir/"
ln -s usr/bin/mitschnitt-app "$dir/AppRun"
ln -s mitschnitt-app.png "$dir/.DirIcon"
appimagetool --runtime-file /usr/local/share/appimage-runtime "$dir" "$bundle/appimage/Mitschnitt_v${version}_amd64.AppImage"
