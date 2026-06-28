# ESP32-S3 networked eyes

Bare-metal (`no_std`) Rust firmware for the **Adafruit ESP32-S3 TFT Feather**: a
pair of animated eyes on the built-in ST7789 that normally blink, and squeeze
shut and **struggle** when triggered over WiFi via `GET /toggle-lights`. The
struggle is a placeholder for the real goal — controlling lights.

## Layout

```
esp32/
├── eye-anim/                 # pure, no_std-friendly logic (easing, blink + struggle state machine)
│   └── src/lib.rs            #   -> unit-tested on the host with `cargo test`, no board needed
├── firmware/                 # the ESP32-S3 binary (no_std, built for Xtensa)
│   ├── .cargo/config.toml    #   target + flash runner + build-std
│   ├── build.rs              #   adds esp-hal's linker script (-Tlinkall.x)
│   └── src/bin/lights.rs     #   WiFi + HTTP server + the animation
├── wifi.env.example          # copy to wifi.env (gitignored) and fill in
├── Makefile                  # make test / flash / monitor / clean
└── Cargo.toml                # workspace root
```

The split is deliberate: the animation/timing math has **no hardware
dependencies**, so it lives in `eye-anim` and is tested with a normal
`cargo test`. `lights.rs` wires that logic to the display and the network.

## Prerequisites (already installed on this machine)

- The Espressif Rust toolchain via [`espup`](https://github.com/esp-rs/espup)
  (`espup install` / `espup update`) — provides the `esp` rustc toolchain and the
  Xtensa GCC linker.
- [`espflash`](https://github.com/esp-rs/espflash) **4.x or newer** for flashing
  (esp-hal 1.0 images use an app-descriptor format older espflash mishandles).
- `export-esp.sh` in your home dir (created by `espup`). It puts the Xtensa
  linker on `PATH`. The `Makefile` sources it automatically.

## Test (no hardware)

```bash
make test
```

Runs the `eye-anim` unit tests on your Mac (blink curve, struggle squeeze/tremble,
relief overshoot, transition smoothness).

## Run it

One-time, set your WiFi credentials (kept out of git):

```bash
cp wifi.env.example wifi.env       # then edit wifi.env with your network
```

`wifi.env` is gitignored; the Makefile sources it and the firmware bakes the
credentials in at build time via `option_env!` (never committed, never written to
a filesystem on the device). Then:

```bash
make flash           # build (release) + flash + open the serial monitor
```

Watch the serial output for the assigned address:

```
wifi: connecting to 'your-ssid'
wifi: connected
net: ready! visit  http://10.0.0.136/toggle-lights  (or try http://esp-eyes/toggle-lights)
```

Then, from any device on the same network:

```bash
curl http://10.0.0.136/toggle-lights
```

He smoothly squeezes into the struggle for a few seconds, then does a wide-eyed
**relief** beat and returns to normal blinking.

### Behaviour

- **Resting:** eyes blink smoothly every 1–4 s (with an occasional double-blink).
- **Triggered:** `/toggle-lights` sets a few-second "work" window. He eases into a
  hard squeeze and trembles/clenches in pulses, then on release pops wide with
  relief and settles. State machine + timing live in `eye-anim/src/lib.rs`
  (`Idle → EnterWork → Working → ExitWork → Relief → Idle`).

The HTTP handler is intentionally tiny (`http_server` in `lights.rs`); the trigger
is just a shared atomic deadline the animation loop reads, so it's easy to point
at real light control later. Networking is esp-radio + esp-rtos + embassy-net.

### A stable address (DHCP reservation)

The device uses DHCP, so its IP can change. The clean, OS-independent fix is a
**DHCP reservation** in your router: bind the device's MAC to a fixed IP, then
always use that IP. The firmware sends a DHCP hostname (`esp-eyes`, the `HOSTNAME`
constant in `lights.rs`) so it's easy to spot in the router's device list.

(mDNS / `esp-eyes.local` is the alternative, but it's inconsistent across
platforms and slow on macOS — it waits ~5s for an IPv6 answer the device doesn't
send — so a reservation is preferred.)

## Hardware (Adafruit ESP32-S3 TFT Feather, built-in ST7789)

| Signal | SPI clock | MOSI | CS | DC | Reset | Backlight | Panel power |
|--------|-----------|------|----|----|-------|-----------|-------------|
| GPIO   | 36        | 35   | 7  | 39 | 40    | 45        | 21          |

For a different board, change those pins (and `ROTATION` / `PANEL_OFFSET`) near
the top of `lights.rs`. Eye geometry and blink timing are in `eye-anim/src/lib.rs`.

## Troubleshooting the USB connection

`espflash` needs a serial port (e.g. `/dev/cu.usbmodemXXX` on macOS). If flashing
can't find the board:

- Use a **data** USB cable (not charge-only).
- Replug; if macOS sees the USB device but never creates `/dev/cu.usbmodem*`, the
  cable/port likely isn't passing data — try another. Check with
  `system_profiler SPUSBDataType | grep -A4 JTAG`.
- If needed, hold **BOOT**, tap **RESET**, release **BOOT** to force download mode.
