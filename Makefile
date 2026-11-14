MODEL := models/ggml-base.en.bin

.PHONY: dev build run test cli model

dev: $(MODEL)    ## Debug build, opens the GUI
	cargo run -- gui

build:   ## Optimized single binary in target/release (or $CARGO_TARGET_DIR/release)
	cargo build --release

run: $(MODEL) ## Build and launch the release GUI
	cargo run --release -- gui

test:
	cargo test

cli: $(MODEL)    ## Guided terminal session (same as `run`)
	cargo run -- run

model: $(MODEL) ## Download the Whisper speech-to-text model (~148 MB, once)

$(MODEL):
	mkdir -p models
	curl -L --fail -o $@.part https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.en.bin
	mv $@.part $@
