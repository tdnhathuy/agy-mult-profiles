.PHONY: build test install clean

build:
	cargo build --release

test:
	cargo test

install: build
	mkdir -p $(HOME)/.local/bin
	# Replace (not overwrite) the binary: copying over the same inode leaves macOS with a stale
	# cached code signature and the new binary is killed with SIGKILL on launch.
	rm -f $(HOME)/.local/bin/agyp
	cp target/release/agyp $(HOME)/.local/bin/agyp
	chmod +x $(HOME)/.local/bin/agyp
	@echo "Installed agyp to $(HOME)/.local/bin/agyp"
	rm -f $(HOME)/.local/bin/codexp
	cp target/release/codexp $(HOME)/.local/bin/codexp
	chmod +x $(HOME)/.local/bin/codexp
	@echo "Installed codexp to $(HOME)/.local/bin/codexp"

clean:
	cargo clean
