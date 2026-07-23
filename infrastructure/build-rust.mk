# Shared cargo-lambda build macros for Rust Lambda functions (Task 39).
#
# The function packages live as standalone Cargo workspaces under functions/
# with path dependencies on ../../crates/*. SAM's makefile builder copies the
# CodeUri (the repository root — see infrastructure/template.yaml) into a
# sandbox, so the whole workspace is present and path dependencies resolve.
#
# The root Makefile includes this file and declares `build-<LogicalId>`
# targets. SAM invokes `make build-<LogicalId>` with `ARTIFACTS_DIR` set to a
# per-function staging directory. The provided.al2023 runtime expects a
# `bootstrap` executable at the root of that directory.
#
# cargo-lambda lays out `--bin <name>` output as `<lambda-dir>/<name>/bootstrap`;
# the macro copies that to `$(ARTIFACTS_DIR)/bootstrap`.
#
#   $(call cargo-lambda-bin,<bin-name>,<package-dir>)

CARGO_LAMBDA ?= cargo lambda
RUST_BUILD_FLAGS ?= --release --x86-64 --output-format binary
SAM_CARGO_TARGET_DIR ?= $(abspath $(ARTIFACTS_DIR)/../cargo-target)

define cargo-lambda-bin
	cd $(2) && CARGO_TARGET_DIR="$(SAM_CARGO_TARGET_DIR)" $(CARGO_LAMBDA) build $(RUST_BUILD_FLAGS) --lambda-dir "$(ARTIFACTS_DIR).clstage" --bin $(1)
	mkdir -p "$(ARTIFACTS_DIR)"
	cp "$(ARTIFACTS_DIR).clstage/$(1)/bootstrap" "$(ARTIFACTS_DIR)/bootstrap"
	rm -rf "$(ARTIFACTS_DIR).clstage"
endef
