.PHONY: build test install clean

build:
	cargo build --release

test:
	cargo test

install: build
	mkdir -p $(HOME)/.local/bin
	cp target/release/agyp $(HOME)/.local/bin/agyp
	chmod +x $(HOME)/.local/bin/agyp
	@echo "Installed agyp to $(HOME)/.local/bin/agyp"

clean:
	cargo clean
