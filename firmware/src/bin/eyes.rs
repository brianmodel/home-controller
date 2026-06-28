//! Smooth eye-blinking + "struggle" animation on the Adafruit ESP32-S3 TFT
//! Feather's built-in 240x135 ST7789 display.
//!
//! Resting state: the eyes blink occasionally. Press the onboard **BOOT** button
//! (GPIO0) to toggle the *working* state — he squeezes his eyes shut hard and
//! trembles like he's straining at a task. Press again to relax back to normal
//! blinking. All transitions are smoothly eased (see the host-tested `eye-anim`).
//!
//! The same toggle is just `Eyes::set_working(bool)`, so it can later be driven
//! by a network / home-controller signal instead of the button.
#![no_std]
#![no_main]

use esp_backtrace as _;
use esp_hal::clock::CpuClock;
use esp_hal::delay::Delay;
use esp_hal::gpio::{Input, InputConfig, Level, Output, OutputConfig, Pull};
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

use eye_anim::{EyeConfig, EyeFrame, EyeRender, Eyes};

esp_bootloader_esp_idf::esp_app_desc!();

// ---- Adafruit ESP32-S3 TFT Feather, built-in ST7789 pinout -----------------
// SCK=36 MOSI=35 CS=7 DC=39 RST=40, backlight=45, panel power=21, BOOT button=0.
const PANEL_OFFSET: (u16, u16) = (52, 40);
const ROTATION: Rotation = Rotation::Deg270;

// Each eye is drawn inside a "cell" a bit larger than the eye, so the eye can
// move (tremble) and grow (the wide-eyed relief beat) within it while we still
// blit the same fixed rectangle each frame (which is what keeps it flicker-free).
// Margin covers tremble (±3px) plus the relief overshoot (~25% bigger).
const CELL_W: usize = EyeConfig::feather_tft().eye_w as usize + 16;
const CELL_H: usize = EyeConfig::feather_tft().eye_h as usize + 20;

#[main]
fn main() -> ! {
    let p = esp_hal::init(esp_hal::Config::default().with_cpu_clock(CpuClock::max()));
    let mut delay = Delay::new();

    // Power the panel and turn the backlight on.
    let _panel_power = Output::new(p.GPIO21, Level::High, OutputConfig::default());
    let _backlight = Output::new(p.GPIO45, Level::High, OutputConfig::default());

    // BOOT button: active-low, internal pull-up.
    let button = Input::new(p.GPIO0, InputConfig::default().with_pull(Pull::Up));

    // SPI bus on the display's SCK/MOSI pins @ 80 MHz.
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

    let seed = Rng::new().random();
    let cfg = EyeConfig::feather_tft();
    let mut eyes = Eyes::new(cfg, seed, 0);

    // One reusable cell framebuffer (drawn + blitted for each eye in turn).
    let mut cell = [Rgb565::BLACK; CELL_W * CELL_H];

    // Only repaint when the frame changes (rock-steady while open & idle; repaints
    // every tick while blinking / trembling).
    let mut last_frame: Option<EyeFrame> = None;

    // Debounced edge detection for the toggle button.
    let mut prev_low = false;
    let mut last_toggle_ms = 0u64;

    let started = Instant::now();
    loop {
        let now_ms = started.elapsed().as_millis();

        // Toggle working state on a fresh button press.
        let low = button.is_low();
        if low && !prev_low && now_ms.saturating_sub(last_toggle_ms) > 250 {
            let next = !eyes.is_working();
            eyes.set_working(next);
            last_toggle_ms = now_ms;
        }
        prev_low = low;

        let frame = eyes.update(now_ms);
        if last_frame != Some(frame) {
            render_eye(&mut display, &mut cell, cfg.left_center(), frame.left);
            render_eye(&mut display, &mut cell, cfg.right_center(), frame.right);
            last_frame = Some(frame);
        }

        delay.delay_millis(4);
    }
}

/// Render one eye into the cell framebuffer and blit the whole cell to its fixed
/// screen rectangle (centered on the eye's resting position) in one SPI write.
fn render_eye<D>(display: &mut D, cell: &mut [Rgb565; CELL_W * CELL_H], nominal: (i32, i32), eye: EyeRender)
where
    D: DrawTarget<Color = Rgb565>,
{
    let white = PrimitiveStyle::with_fill(Rgb565::WHITE);

    // Top-left of the cell in screen coordinates (fixed at the resting center).
    let cell_x = nominal.0 - CELL_W as i32 / 2;
    let cell_y = nominal.1 - CELL_H as i32 / 2;

    // Draw into RAM: clear, then the (possibly offset/squished) ellipse.
    {
        let mut fb = FrameBuf::new(&mut *cell, CELL_W, CELL_H);
        let _ = fb.clear(Rgb565::BLACK);
        let ex = (eye.cx - eye.w / 2) - cell_x;
        let ey = (eye.cy - eye.h / 2) - cell_y;
        let _ = Ellipse::new(Point::new(ex, ey), Size::new(eye.w as u32, eye.h as u32))
            .into_styled(white)
            .draw(&mut fb);
    }

    // Blit the cell to its fixed position.
    let area = Rectangle::new(
        Point::new(cell_x, cell_y),
        Size::new(CELL_W as u32, CELL_H as u32),
    );
    let _ = display.fill_contiguous(&area, cell.iter().copied());
}
