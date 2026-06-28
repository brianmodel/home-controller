//! Simplest possible "is it alive?" firmware for the ESP32-S3.
//!
//! Every 500 ms it does two independent things:
//!   1. Prints a heartbeat line over USB serial (visible in the monitor).
//!   2. Toggles a GPIO pin (drive an LED from `LED_PIN` if your board has one).
//!
//! The serial heartbeat is the reliable verification: you'll see it in
//! `espflash`'s monitor no matter which LED (if any) your board has.
#![no_std]
#![no_main]

use esp_backtrace as _;
use esp_hal::clock::CpuClock;
use esp_hal::delay::Delay;
use esp_hal::gpio::{Level, Output, OutputConfig};
use esp_hal::main;
use log::info;

// The ESP32-S3-DevKitC-1 has an addressable RGB LED on GPIO48 (toggling it as a
// plain output won't show a clean color, but is harmless). If your board has a
// plain LED on a different pin, change the pin used in `Output::new` below.
const LED_PIN: u8 = 48;

// esp-hal 1.0 firmware must embed an esp-idf app descriptor so the second-stage
// bootloader will accept and boot the image.
esp_bootloader_esp_idf::esp_app_desc!();

#[main]
fn main() -> ! {
    // Bring up the chip at max CPU clock.
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);

    // Route the `log` crate to USB serial.
    esp_println::logger::init_logger_from_env();

    info!("ESP32-S3 is alive! Heartbeat + blink on GPIO{LED_PIN}.");

    let mut led = Output::new(peripherals.GPIO48, Level::Low, OutputConfig::default());
    let delay = Delay::new();

    let mut tick: u32 = 0;
    loop {
        led.toggle();
        info!("tick {tick}");
        tick = tick.wrapping_add(1);
        delay.delay_millis(500);
    }
}
