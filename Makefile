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

.PHONY: all build release go-lib run-demo run-demo-node run-demo-python clean

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

clean:
	cargo clean
	go clean ./...
