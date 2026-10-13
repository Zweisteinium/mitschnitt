.PHONY: test test-live dev build appimage

test:
	cargo test --workspace

# Silent capture test against the running PipeWire session (Linux).
test-live:
	cargo test -p mitschnitt-core stream_is_captured_bit_exact -- --ignored

dev:
	cd app && npm install && npx tauri dev

build:
	cd app && npm install && npx tauri build

# Portable AppImage, built in Debian 12: target/appimage/release/bundle/appimage
appimage:
	docker build -t mitschnitt-build packaging/linux
	docker run --rm --user $$(id -u):$$(id -g) -v "$$PWD":/src -w /src/app -e HOME=/src/target/appimage/home \
		-e CARGO_HOME=/src/target/appimage/cargo -e CARGO_TARGET_DIR=/src/target/appimage \
		mitschnitt-build sh -c 'npm ci && npx tauri build --bundles appimage'
