# side-huddle build & run targets
#
# Usage:
#   make              — build everything
#   make run-demo     — run the Go demo
#   make run-demo-node   — run the Node.js demo
#   make run-demo-python — run the Python demo
#   make clean        — remove build artifacts

NODE_DIR  := crates/side-huddle-node
DYLIB_DIR := target/release

.PHONY: all build release run-demo run-demo-node run-demo-python clean

all: build

## Debug Rust build + verify Go compiles
build:
	cargo build
	go build ./...

## napi-rs Node.js addon (release) — also builds the Rust library
release:
	cd $(NODE_DIR) && npx napi build --platform --release

## Run the Go demo (builds release first)
run-demo: release
	DYLD_LIBRARY_PATH=$(DYLIB_DIR):$$DYLD_LIBRARY_PATH \
	LD_LIBRARY_PATH=$(DYLIB_DIR):$$LD_LIBRARY_PATH \
		go run ./cmd/demo

## Run the Node.js demo (builds release first)
run-demo-node: release
	node bindings/node/demo.js

## Run the Python demo (no build step needed — pure ctypes)
run-demo-python:
	python3 bindings/python/demo.py

clean:
	cargo clean
	go clean ./...
