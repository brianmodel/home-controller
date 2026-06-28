# Convenience wrapper around cargo for this ESP32-S3 project.
#
#   make test      run host unit tests for the animation logic (no hardware)
#   make flash     build + flash + open the serial monitor (needs wifi.env)
#   make monitor   just open the serial monitor on the connected board
#   make clean     remove build artifacts
#
# `make flash` sources two things for you:
#   - ~/export-esp.sh  (puts the Xtensa GCC linker on PATH; created by espup)
#   - ./wifi.env       (WiFi credentials; copy wifi.env.example first)

ESP_ENV  := $(HOME)/export-esp.sh
WIFI_ENV := wifi.env
ESP      := . $(ESP_ENV) >/dev/null 2>&1;
WIFI     := . ./$(WIFI_ENV);

.PHONY: test flash monitor clean

test:
	cargo test

flash:
	@[ -f $(WIFI_ENV) ] || { echo "Missing $(WIFI_ENV). Run: cp wifi.env.example wifi.env  and fill it in."; exit 1; }
	$(WIFI) $(ESP) cd firmware && cargo run --release

monitor:
	espflash monitor

clean:
	cargo clean
