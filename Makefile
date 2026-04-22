# side-huddle build & distribution
#
# ── Dev workflow ──────────────────────────────────────────────────────────────
#   make                  debug build (cargo + go)
#   make run-demo         run the Go demo from the terminal
#   make bundle           .app bundle (ad-hoc signed, for your machine only)
#   make run-bundle       bundle → open via LaunchServices
#   make install          bundle → /Applications (local dev)
#
# ── Release workflow ──────────────────────────────────────────────────────────
#   git tag v0.3.0 && git push origin v0.3.0
#
#   GitHub Actions (.github/workflows/release-macos.yml) fires automatically
#   and runs packaging/macos/package.sh with your stored secrets.
#
#   To do a release locally (e.g. to test the full pipeline):
#     APPLE_CERTIFICATE_P12=<base64>        \
#     APPLE_CERTIFICATE_PASSWORD=<pass>     \
#     APPLE_APP_PASSWORD=<app-specific-pw>  \
#     make dist
#
# ─────────────────────────────────────────────────────────────────────────────

# Local overrides: copy config.mk.example → config.mk and fill in your values.
# config.mk is gitignored; it never leaves your machine.
-include config.mk

NODE_DIR    := crates/side-huddle-node
DYLIB_DIR   := target/release
GO_LIB_DIR  := bindings/go/lib/darwin_arm64

APP_DIR     := dist/SideHuddle.app
INSTALL_DIR := /Applications/SideHuddle.app
INFO_PLIST  := tools/bundle/Info.plist
ENTITLEMENTS:= tools/bundle/side-huddle.entitlements

# Ad-hoc signing identity for local dev bundles.
# package.sh uses the hardcoded Developer ID Application for real releases.
SIGN_ID     ?= -

# Version: prefer the latest git tag, else Cargo workspace version.
APP_VERSION ?= $(shell git describe --tags --abbrev=0 2>/dev/null \
                 | sed 's/^v//' \
                 || grep '^version' Cargo.toml | head -1 \
                      | sed 's/.*"\(.*\)".*/\1/')

.PHONY: all build release go-lib run-demo run-demo-node run-demo-python \
        bundle run-bundle install icon verify dist clean

# ── Source targets ────────────────────────────────────────────────────────────

icon:
	tools/bundle/make-icon.sh

tools/bundle/SideHuddle.icns: tools/bundle/make-icon.sh
	tools/bundle/make-icon.sh

all: build

build:
	cargo build
	go build ./...

release:
	cd $(NODE_DIR) && npx napi build --platform --release

go-lib:
	cargo build --release -p side-huddle
	cp $(DYLIB_DIR)/libside_huddle.a $(GO_LIB_DIR)/libside_huddle.a

DEMO_BIN := /tmp/side-huddle-demo

run-demo: go-lib release
	go build -a -o $(DEMO_BIN) ./cmd/demo
	$(DEMO_BIN)

run-demo-node: release
	node bindings/node/demo.js

run-demo-python:
	python3 bindings/python/demo.py

# ── Local dev bundle (ad-hoc signed, your machine only) ──────────────────────

bundle: go-lib tools/bundle/SideHuddle.icns
	rm -rf $(APP_DIR)
	mkdir -p $(APP_DIR)/Contents/MacOS $(APP_DIR)/Contents/Resources
	go build -a -o $(APP_DIR)/Contents/MacOS/SideHuddle ./cmd/demo
	cp $(INFO_PLIST) $(APP_DIR)/Contents/Info.plist
	cp tools/bundle/SideHuddle.icns $(APP_DIR)/Contents/Resources/SideHuddle.icns
	plutil -replace CFBundleIdentifier          -string "com.ms.side-huddle"  $(APP_DIR)/Contents/Info.plist
	plutil -replace CFBundleShortVersionString  -string "$(APP_VERSION)"      $(APP_DIR)/Contents/Info.plist
	codesign --force --options runtime          \
		--entitlements $(ENTITLEMENTS)          \
		-s "$(SIGN_ID)"                         \
		$(APP_DIR)
	@echo "── signature ─────────────────────────────────"
	@codesign -dvv $(APP_DIR) 2>&1 | grep -E "Identifier|Authority|TeamIdentifier|flags" || true
	@echo "──────────────────────────────────────────────"

verify:
	codesign --verify --deep --strict --verbose=2 $(APP_DIR)
	spctl --assess --type exec --verbose $(APP_DIR) 2>&1 || \
	  echo "(spctl fails on ad-hoc / un-notarized bundles — expected for local builds)"

run-bundle: bundle
	open $(APP_DIR) --stdout /tmp/side-hustle.log --stderr /tmp/side-hustle.log
	@echo "launched — tail: tail -f /tmp/side-hustle.log"

install: bundle
	-pkill -9 SideHuddle 2>/dev/null || true
	@sleep 1
	rm -rf "$(INSTALL_DIR)"
	ditto "$(APP_DIR)" "$(INSTALL_DIR)"
	@echo "✓ installed to $(INSTALL_DIR)"

# ── Release (sign + notarize + staple + DMG) ─────────────────────────────────
# Delegates to packaging/macos/package.sh which contains the full pipeline.
# Requires APPLE_CERTIFICATE_P12, APPLE_CERTIFICATE_PASSWORD, APPLE_APP_PASSWORD.

dist:
	bash packaging/macos/package.sh arm64 "$(APP_VERSION)"

# ── Housekeeping ──────────────────────────────────────────────────────────────

clean:
	cargo clean
	go clean ./...
	rm -rf dist
