# SAM copies infrastructure/spikes/ (CodeUri: ".") to a temp directory.
# Derive the actual checkout root from the SAM artifacts directory so we
# can build against the workspace without copying the entire repo into the
# sandbox.

REPO_ROOT := $(abspath $(ARTIFACTS_DIR)/../../..)

build-RustBuildSpike:
	cd "$(REPO_ROOT)/functions/build-spike" && cargo lambda build --release --x86-64 --output-format binary --lambda-dir "$(ARTIFACTS_DIR)" --flatten build-spike
