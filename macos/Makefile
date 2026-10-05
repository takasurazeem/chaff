# Build the native macOS shell.
#
# The engine is Rust and the shell is Swift, so this has to do both in the right order: cargo
# produces the static library and the generated bindings, then SwiftPM links them.

CARGO_PROFILE ?= release
TARGET_DIR := $(shell cd .. && pwd)/target
LIB := $(TARGET_DIR)/$(CARGO_PROFILE)/libchaff_ffi.a
# Two destinations, and getting this wrong is a confusing failure: `systemLibrary` targets
# carry a modulemap and a header and **cannot contain Swift**, so the generated bindings have to
# land in the executable target while the C declarations go to the module.
C_MODULE := Sources/ChaffFFI
SWIFT_TARGET := Sources/Chaff

.PHONY: all bindings build app run clean test

all: build

## Generate the Swift bindings from the built library.
##
## Runs after cargo, because `uniffi-bindgen` reads the library's metadata — generating from
## source would need the scaffolding re-parsed and is a different, worse path.
bindings:
	@mkdir -p $(C_MODULE) $(SWIFT_TARGET)
	# Clean first. uniffi writes its own `chaff_ffiFFI.modulemap` beside the header, and two
	# modulemaps in one directory is how SwiftPM resolves a module to the wrong name — which
	# presents as "unable to resolve module dependency", a message that says nothing about the
	# cause.
	@rm -f $(C_MODULE)/chaff_ffiFFI.modulemap $(C_MODULE)/chaff_ffi.swift
	# **Cargo runs every time, not behind a file target.**
	#
	# `bindings: $(LIB)` looked right and was wrong: `$(LIB)` existed, so make never rebuilt
	# it, and `uniffi-bindgen` then read a **stale** library and regenerated the same old Swift.
	# The symptom is `cannot find type 'PhotoDetail' in scope` for a type that is plainly in the
	# Rust — a failure that points at the Swift and not at the build order.
	#
	# Cargo's own fingerprinting is better at this than a Makefile rule.
	cd .. && cargo build -p chaff-ffi --profile $(CARGO_PROFILE)
	cargo run --manifest-path ../crates/chaff-ffi/Cargo.toml --bin uniffi-bindgen -- \
		generate --library $(LIB) --language swift --out-dir $(SWIFT_TARGET)
	@mv $(SWIFT_TARGET)/chaff_ffiFFI.h $(C_MODULE)/
	@mv $(SWIFT_TARGET)/chaff_ffiFFI.modulemap $(C_MODULE)/module.modulemap
	@echo "generated: swift -> $(SWIFT_TARGET), C -> $(C_MODULE)"

build: bindings
	swift build -c $(if $(filter release,$(CARGO_PROFILE)),release,debug)

## Package it as a `.app`, which is what makes it launchable.
app: build
	./bundle.sh $(if $(filter release,$(CARGO_PROFILE)),release,debug)

run: app
	open build/Chaff.app

test: bindings
	swift test

## Everything that has to be green before this is worth shipping.
##
## The engine's tests are not run here — they belong to cargo, and `cargo test --workspace` at
## the repository root covers them. This is the shell.
check: test
	swift build -c release

clean:
	rm -rf .build $(SWIFT_TARGET)/chaff_ffi.swift $(C_MODULE)/chaff_ffiFFI.h $(C_MODULE)/module.modulemap
