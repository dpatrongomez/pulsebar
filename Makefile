# Common tasks. Signing/notarization: pass SIGN_ID=… and NOTARY_PROFILE=… (see packaging/bundle.sh).
VERSION := $(shell sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)

.PHONY: build app dmg install install-linux test clean

build:            ## release binary → target/release/pulsebar
	cargo build --release

app:              ## dist/PulseBar.app
	packaging/bundle.sh app

dmg:              ## dist/PulseBar-$(VERSION).dmg (installable: drag to Applications)
	packaging/bundle.sh dmg

install: app      ## copy the app to /Applications
	rm -rf /Applications/PulseBar.app
	ditto dist/PulseBar.app /Applications/PulseBar.app
	@echo "installed /Applications/PulseBar.app"

PREFIX ?= $(HOME)/.local

install-linux:    ## binary, launcher and icon under PREFIX (default ~/.local)
	cargo build --release
	install -Dm755 target/release/pulsebar $(PREFIX)/bin/pulsebar
	install -Dm644 packaging/linux/pulsebar.desktop $(PREFIX)/share/applications/pulsebar.desktop
	install -Dm644 docs/icon.png $(PREFIX)/share/icons/hicolor/256x256/apps/pulsebar.png
	@echo "installed to $(PREFIX); run: pulsebar window"

test:
	cargo test

clean:
	cargo clean
	rm -rf dist
