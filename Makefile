# side-huddle build & run targets
#
# Usage:
#   make              — build everything
#   make run-demo     — run the Go demo
#   make run-demo-node   — run the Node.js demo
#   make run-demo-python — run the Python demo
#   make clean        — remove build artifacts

NODE_DIR    := crates/side-huddle-node
DYLIB_DIR   := target/release
GO_LIB_DIR  := bindings/go/lib/darwin_arm64

APP_DIR      := dist/SideHuddle.app
# Override BUNDLE_ID and SIGN_ID on the command line or via env for a stable
# TCC designated requirement tied to your own Developer ID. The defaults here
# produce an ad-hoc signed bundle that works for local testing — grants will
# reset on every rebuild because ad-hoc signatures have no stable DR.
#
#   make bundle \
#     BUNDLE_ID=com.acme.sidehuddle \
#     SIGN_ID="Developer ID Application: Acme Inc (TEAMID00)"
BUNDLE_ID    ?= com.example.sidehuddle
SIGN_ID      ?= -
INFO_PLIST   := tools/bundle/Info.plist
ENTITLEMENTS := tools/bundle/side-huddle.entitlements

.PHONY: all build release go-lib run-demo run-demo-node run-demo-python bundle run-bundle icon clean

## Regenerate the app icon from the Pillow-drawn 1024x1024 base.
icon:
	tools/bundle/make-icon.sh

tools/bundle/SideHuddle.icns: tools/bundle/make-icon.sh
	tools/bundle/make-icon.sh

all: build

## Debug Rust build + verify Go compiles
build:
	cargo build
	go build ./...

## napi-rs Node.js addon (release) — also builds the Rust library
release:
	cd $(NODE_DIR) && npx napi build --platform --release

## Build the static archive used by the Go binding (darwin/arm64)
go-lib:
	cargo build --release -p side-huddle
	cp $(DYLIB_DIR)/libside_huddle.a $(GO_LIB_DIR)/libside_huddle.a

## Run the Go demo (rebuilds static archive + node addon first)
## Uses `go build -a` to force CGo to relink the fresh archive — avoids
## stale-cache bugs where `go run` skips relinking after archive changes.
DEMO_BIN := /tmp/side-huddle-demo

run-demo: go-lib release
	go build -a -o $(DEMO_BIN) ./cmd/demo
	$(DEMO_BIN)

## Run the Node.js demo (builds release first)
run-demo-node: release
	node bindings/node/demo.js

## Run the Python demo (no build step needed — pure ctypes)
run-demo-python:
	python3 bindings/python/demo.py

## Build a codesigned .app bundle so TCC (Screen Recording / Microphone)
## attaches to a stable designated requirement instead of whatever terminal
## hosts the CLI. With a real Developer ID, TCC grants survive rebuilds.
bundle: go-lib tools/bundle/SideHuddle.icns
	rm -rf $(APP_DIR)
	mkdir -p $(APP_DIR)/Contents/MacOS $(APP_DIR)/Contents/Resources
	go build -a -o $(APP_DIR)/Contents/MacOS/SideHuddle ./cmd/demo
	cp $(INFO_PLIST) $(APP_DIR)/Contents/Info.plist
	cp tools/bundle/SideHuddle.icns $(APP_DIR)/Contents/Resources/SideHuddle.icns
	plutil -replace CFBundleIdentifier -string "$(BUNDLE_ID)" \
		$(APP_DIR)/Contents/Info.plist
	codesign --force --options runtime \
		--entitlements $(ENTITLEMENTS) \
		-s "$(SIGN_ID)" \
		$(APP_DIR)
	@echo "--- signature ---"
	@codesign -dvv $(APP_DIR) 2>&1 | grep -E "Identifier|Authority|TeamIdentifier" || true

## Launch the signed bundle via LaunchServices so TCC attributes perms to
## the bundle, not the hosting terminal. Direct-exec of the inner binary
## makes TCC walk up to the parent shell — the wrong attribution.
run-bundle: bundle
	open $(APP_DIR) --stdout /tmp/side-huddle.log --stderr /tmp/side-huddle.log
	@echo "launched via LaunchServices — tail with: tail -f /tmp/side-huddle.log"

clean:
	cargo clean
	go clean ./...
	rm -rf dist
