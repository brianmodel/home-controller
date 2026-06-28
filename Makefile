# Convenience wrapper around cargo for this ESP32-S3 project.
#
# The firmware targets the Xtensa core, whose linker (xtensa-esp32s3-elf-gcc)
# lives in the toolchain that `espup` installed. `export-esp.sh` puts it on PATH.
# These targets source it for you so you never have to remember to.
#
#   make test      run host unit tests (no hardware needed)
#   make build     compile the firmware (debug)
#   make release   compile the firmware (release, optimized)
#   make flash     build + flash + open serial monitor  (alias: make run)
#   make eyes      build + flash the smooth eye-blinking animation
#   make lights    build + flash the WiFi build (needs wifi.env); /toggle-lights
#   make blinky    build + flash the simple blinky/heartbeat
#   make monitor   just open the serial monitor on the connected board
#   make check     fast type-check of the firmware
#   make clean     remove build artifacts

ESP_ENV  := $(HOME)/export-esp.sh
WIFI_ENV := wifi.env
# `. $(ESP_ENV)` sources the Espressif env (PATH to the Xtensa GCC, etc.)
ESP      := . $(ESP_ENV) >/dev/null 2>&1;
# Source WiFi creds if present (only needed by the `lights` binary).
WIFI     := [ -f $(WIFI_ENV) ] && . ./$(WIFI_ENV);
FW       := cd firmware &&

.PHONY: test build release flash run eyes blinky lights monitor check clean

test:
	cargo test

build:
	$(ESP) $(FW) cargo build

release:
	$(ESP) $(FW) cargo build --release

flash run:
	$(ESP) $(FW) cargo run --release

eyes:
	$(ESP) $(FW) cargo run --release --bin eyes

blinky:
	$(ESP) $(FW) cargo run --release --bin blinky

# Networked build: needs WiFi creds from wifi.env (copy wifi.env.example first).
lights:
	@[ -f $(WIFI_ENV) ] || { echo "Missing $(WIFI_ENV). Run: cp wifi.env.example wifi.env  and fill it in."; exit 1; }
	$(WIFI) $(ESP) $(FW) cargo run --release --bin lights

monitor:
	espflash monitor

check:
	$(ESP) $(FW) cargo check

clean:
	cargo clean
