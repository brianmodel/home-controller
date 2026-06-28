//! Smooth eye-blinking animation on the Adafruit ESP32-S3 TFT Feather's
//! built-in 240x135 ST7789 display.
//!
//! The blink timing/easing lives in the host-tested `eye-anim` crate; this file
//! just owns the hardware: power the panel, bring up SPI + the ST7789, and draw
//! the two ellipses each tick.
#![no_std]
#![no_main]

use esp_backtrace as _;
use esp_hal::clock::CpuClock;
use esp_hal::delay::Delay;
use esp_hal::gpio::{Level, Output, OutputConfig};
use esp_hal::main;
use esp_hal::rng::Rng;
use esp_hal::spi::master::{Config as SpiConfig, Spi};
use esp_hal::spi::Mode;
use esp_hal::time::{Instant, Rate};

use embedded_graphics::pixelcolor::Rgb565;
use embedded_graphics::prelude::*;
use embedded_graphics::primitives::{Ellipse, PrimitiveStyle, Rectangle};
use embedded_graphics_framebuf::FrameBuf;

use embedded_hal_bus::spi::ExclusiveDevice;
use mipidsi::interface::SpiInterface;
use mipidsi::options::{ColorInversion, Orientation, Rotation};
use mipidsi::{models::ST7789, Builder};

use eye_anim::{EyeConfig, Eyes};

esp_bootloader_esp_idf::esp_app_desc!();

// ---- Adafruit ESP32-S3 TFT Feather, built-in ST7789 pinout -----------------
// (from the board's Arduino variant: SCK=36 MOSI=35 CS=7 DC=39 RST=40,
//  backlight=45, panel power=21)
// The display is a 1.14" 240x135 panel. mipidsi is told the controller's native
// 135x240 window plus this panel's offset, then rotated into landscape.
// If the image is shifted or rotated, tweak OFFSET / ROTATION below.
const PANEL_OFFSET: (u16, u16) = (52, 40);
const ROTATION: Rotation = Rotation::Deg270;

#[main]
fn main() -> ! {
    let p = esp_hal::init(esp_hal::Config::default().with_cpu_clock(CpuClock::max()));
    let mut delay = Delay::new();

    // Power the panel and turn the backlight on (held high for the program's life).
    let _panel_power = Output::new(p.GPIO21, Level::High, OutputConfig::default());
    let _backlight = Output::new(p.GPIO45, Level::High, OutputConfig::default());

    // SPI bus on the display's SCK/MOSI pins.
    let spi = Spi::new(
        p.SPI2,
        SpiConfig::default()
            .with_frequency(Rate::from_mhz(80))
            .with_mode(Mode::_0),
    )
    .unwrap()
    .with_sck(p.GPIO36)
    .with_mosi(p.GPIO35);

    let cs = Output::new(p.GPIO7, Level::High, OutputConfig::default());
    let spi_device = ExclusiveDevice::new(spi, cs, Delay::new()).unwrap();

    let dc = Output::new(p.GPIO39, Level::Low, OutputConfig::default());
    let rst = Output::new(p.GPIO40, Level::Low, OutputConfig::default());

    let mut if_buffer = [0u8; 512];
    let di = SpiInterface::new(spi_device, dc, &mut if_buffer);

    let mut display = Builder::new(ST7789, di)
        .reset_pin(rst)
        .display_size(135, 240)
        .display_offset(PANEL_OFFSET.0, PANEL_OFFSET.1)
        .orientation(Orientation::new().rotate(ROTATION))
        .invert_colors(ColorInversion::Inverted)
        .init(&mut delay)
        .unwrap();

    display.clear(Rgb565::BLACK).unwrap();

    // Seed the blink RNG from the hardware RNG so the idle pattern differs each run.
    let seed = Rng::new().random();

    let cfg = EyeConfig::feather_tft();
    let mut eyes = Eyes::new(cfg, seed, 0);

    // Reusable off-screen buffer for one eye's bounding box. We render the eye
    // into RAM here (clear + ellipse) and then blit the whole box to the panel in
    // a single contiguous SPI write. Because the panel pixels go straight from the
    // old eye to the new one (never flashing black in between), there is no flicker.
    const EW: usize = EyeConfig::feather_tft().eye_w as usize;
    const EH: usize = EyeConfig::feather_tft().eye_h as usize;
    let mut eye_buf = [Rgb565::BLACK; EW * EH];

    // Track the last drawn height so we only repaint when it changes — rock-steady
    // while open, repaint only mid-blink.
    let mut last_h = -1;
    let started = Instant::now();

    loop {
        let now_ms = started.elapsed().as_millis();
        let frame = eyes.update(now_ms);

        if frame.left_h != last_h {
            render_eye(&mut display, &mut eye_buf, cfg.left_center(), EW, EH, frame.left_h);
            render_eye(&mut display, &mut eye_buf, cfg.right_center(), EW, EH, frame.right_h);
            last_h = frame.left_h;
        }

        // Fast tick → many animation steps across the 80ms blink (≈20 frames),
        // which combined with the flicker-free blit gives smooth motion.
        delay.delay_millis(4);
    }
}

/// Render one eye into `buf` (clear + centered white ellipse) and blit the whole
/// box to the panel at `center` in a single contiguous write (flicker-free).
fn render_eye<D, const N: usize>(
    display: &mut D,
    buf: &mut [Rgb565; N],
    center: (i32, i32),
    w: usize,
    max_h: usize,
    cur_h: i32,
) where
    D: DrawTarget<Color = Rgb565>,
{
    let white = PrimitiveStyle::with_fill(Rgb565::WHITE);

    // Draw into the RAM framebuffer (reborrow so `buf` is usable again after).
    {
        let mut fbuf = FrameBuf::new(&mut *buf, w, max_h);
        let _ = fbuf.clear(Rgb565::BLACK);
        let h = cur_h.max(1) as u32;
        // Center the ellipse vertically within the max-height box.
        let top = (max_h as i32 - h as i32) / 2;
        let _ = Ellipse::new(Point::new(0, top), Size::new(w as u32, h))
            .into_styled(white)
            .draw(&mut fbuf);
    }

    // Blit the box to its screen position in one shot.
    let (cx, cy) = center;
    let box_tl = Point::new(cx - w as i32 / 2, cy - max_h as i32 / 2);
    let area = Rectangle::new(box_tl, Size::new(w as u32, max_h as u32));
    let _ = display.fill_contiguous(&area, buf.iter().copied());
}
