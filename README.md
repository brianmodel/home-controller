# ESP32-S3 in Rust

Bare-metal (`no_std`) Rust firmware for the **ESP32-S3**, built with
[`esp-hal`](https://github.com/esp-rs/esp-hal). Designed to be easy to **test**
(pure logic runs on your Mac) and easy to **flash** (one command).

Three firmware binaries:
- **`lights`** — the eyes **plus WiFi + an HTTP server**, so the struggle can be
  triggered remotely (`GET /toggle-lights`). The headline build.
- **`eyes`** — the offline animation: smooth blinking + a BOOT-button struggle,
  in the spirit of
  [esp32-smooth-eye-blinking](https://github.com/dmtrKovalenko/esp32-smooth-eye-blinking).
- **`blinky`** — a minimal serial-heartbeat + LED toggle (the "is it alive?" check).

## Layout

```
esp32/
├── eye-anim/                 # pure, no_std-friendly logic (easing, blink state machine)
│   └── src/lib.rs            #   -> unit-tested on the host with `cargo test`
├── firmware/                 # the ESP32-S3 binaries (no_std, built for Xtensa)
│   ├── .cargo/config.toml    #   target + flash runner + build-std live here
│   ├── build.rs              #   adds esp-hal's linker script (-Tlinkall.x)
│   └── src/bin/
│       ├── eyes.rs           #   smooth eye-blinking animation (ST7789)
│       └── blinky.rs         #   serial heartbeat / LED toggle
├── Makefile                  # one-liners: make test / eyes / blinky / monitor
├── rust-toolchain.toml       # pins the `esp` (Xtensa) toolchain
└── Cargo.toml                # workspace root
```

The split is deliberate: the animation math has **no hardware dependencies**, so
it lives in `eye-anim` and is tested with a normal `cargo test` — no board
required. The firmware in `firmware/` wires that logic to real pins / a display.

## Prerequisites (already installed on this machine)

- The Espressif Rust toolchain via [`espup`](https://github.com/esp-rs/espup)
  (`espup install` / `espup update`). This provides the `esp` rustc toolchain
  and the Xtensa GCC linker.
- [`espflash`](https://github.com/esp-rs/espflash) **4.x or newer** for flashing.
  esp-hal 1.0 images use the esp-idf app-descriptor format that older espflash
  (2.x) mishandles, so use a recent build:
  `cargo install espflash` (or grab a prebuilt binary from the releases page).
- `export-esp.sh` in your home directory (created by `espup`). It puts the
  Xtensa linker on `PATH`. The `Makefile` sources it automatically; if you run
  `cargo` directly, run `. ~/export-esp.sh` first.

## Test (no hardware)

```bash
make test            # or: cargo test
```

Runs the `eye-anim` unit tests on your Mac.

## Run the eyes

Connect the board over USB, then:

```bash
make eyes            # build (release) + flash the eye-blinking animation
```

**Resting state:** two white eyes on black that blink smoothly every 1–4 seconds
(with an occasional quick double-blink).

**Work / struggle:** press the onboard **BOOT** button (GPIO0) to toggle the
*working* state — he smoothly squeezes his eyes shut hard and trembles, clenching
tighter in pulses, like he's straining at a task. Press **BOOT** again and he
releases: his eyes pop **wide with relief**, settle, and return to normal
blinking. Every transition is smoothly eased and you can toggle at any moment.

The trigger is just `Eyes::set_working(bool)`, so the BOOT button can later be
replaced by a network / home-controller signal. The full state machine
(`Idle → EnterWork → Working → ExitWork → Relief → Idle`) and all of its timing
live in `eye-anim/src/lib.rs` and are unit-tested with `cargo test`.

### Hardware: Adafruit ESP32-S3 TFT Feather (built-in ST7789)

`eyes.rs` is wired for this board's built-in 1.14" 240×135 ST7789:

| Signal        | GPIO |
|---------------|------|
| SPI clock     | 36   |
| SPI MOSI      | 35   |
| Chip select   | 7    |
| Data/Command  | 39   |
| Reset         | 40   |
| Backlight     | 45   |
| Panel power   | 21   |

For a different board/display, change those pins in `firmware/src/bin/eyes.rs`.
If the image is rotated or shifted, tweak `ROTATION` / `PANEL_OFFSET` near the
top of that file. Eye geometry and blink timing live in
`eye-anim/src/lib.rs` (`EyeConfig`, and the `CLOSE_MS` / `OPEN_MS` constants) so
you can tune them and re-check with `cargo test`.

## Trigger over WiFi (`lights` binary)

The `lights` binary runs everything `eyes` does **and** joins WiFi and serves a
tiny HTTP endpoint, so the struggle can be triggered remotely — the first step
toward controlling real lights from external systems.

### One-time: WiFi credentials (kept out of git)

```bash
cp wifi.env.example wifi.env       # then edit wifi.env with your network
```

`wifi.env` is gitignored. The Makefile sources it and the firmware bakes the
credentials in at build time via `option_env!` — they are never committed and
never written to a filesystem on the device.

### Run it

```bash
make lights          # build (release) + flash + open the serial monitor
```

Watch the serial output for the assigned IP:

```
wifi: connecting to 'your-ssid'
wifi: connected
net: ready! visit  http://10.0.0.136/toggle-lights
```

Then, from any device on the same network:

```bash
curl http://10.0.0.136/toggle-lights
```

He squeezes into the struggle for a few seconds, then does the relief beat and
returns to normal blinking. The BOOT button still toggles the struggle too.

The endpoint logic is intentionally tiny (`http_server` in
`firmware/src/bin/lights.rs`); the trigger is just an atomic deadline that the
animation loop reads, so it's easy to point at real light control later. The
networking stack is esp-radio + esp-rtos + embassy-net (see `firmware/Cargo.toml`).

## Blinky / serial heartbeat

```bash
make blinky          # build (release) + flash blinky
make monitor         # watch its serial output
```

After it boots you should see, every 500 ms:

```
INFO - ESP32-S3 is alive! Heartbeat + blink on GPIO48.
INFO - tick 0
INFO - tick 1
...
```

That serial heartbeat is the reliable "it works" signal regardless of which LED
your board has. The board also toggles GPIO48 (the RGB LED data pin on the
ESP32-S3-DevKitC-1); change `LED_PIN` / the pin in `firmware/src/bin/blinky.rs`
if your board's LED is elsewhere.

## Troubleshooting the USB connection

`espflash` needs a serial port to appear (e.g. `/dev/cu.usbmodemXXX` on macOS).
If flashing can't find the board:

- Use a **data** USB cable (not charge-only) plugged into the ESP32-S3's USB
  port.
- Replug it; if macOS sees the USB device but never creates a `/dev/cu.usbmodem*`
  node, the cable/port likely isn't passing the data lines cleanly — try another
  cable/port.
- Check it's seen at all: `system_profiler SPUSBDataType | grep -A4 JTAG`.
- If needed, hold **BOOT**, tap **RESET**, release **BOOT** to force download
  mode, then flash.
