//! Networked eyes: the smooth blink/struggle animation, plus a tiny HTTP server
//! so it can be triggered remotely.
//!
//! On boot it joins WiFi (credentials from the `WIFI_SSID` / `WIFI_PASSWORD`
//! build-time env vars — see `wifi.env`), gets an IP over DHCP, and serves on
//! port 80. Hitting **`/toggle-lights`** makes him struggle for a few seconds
//! and then relax on his own:
//!
//!     curl http://<device-ip>/toggle-lights
//!
//! The animation runs concurrently with the network stack. `/toggle-lights` is
//! the placeholder for the real goal — controlling lights.
#![no_std]
#![no_main]

use core::sync::atomic::{AtomicU32, Ordering};

use embassy_executor::Spawner;
use embassy_net::tcp::TcpSocket;
use embassy_net::{IpListenEndpoint, Runner, StackResources};
use embassy_time::{Duration, Instant, Timer};

use esp_backtrace as _;
use esp_hal::clock::CpuClock;
use esp_hal::delay::Delay;
use esp_hal::gpio::{Level, Output, OutputConfig};
use esp_hal::interrupt::software::SoftwareInterruptControl;
use esp_hal::rng::Rng;
use esp_hal::spi::master::{Config as SpiConfig, Spi};
use esp_hal::spi::Mode;
use esp_hal::time::Rate;
use esp_hal::timer::timg::TimerGroup;

use esp_radio::wifi::sta::StationConfig;
use esp_radio::wifi::{Config as WifiConfig, ControllerConfig, Interface, WifiController};

use embedded_graphics::pixelcolor::Rgb565;
use embedded_graphics::prelude::*;
use embedded_graphics::primitives::{Ellipse, PrimitiveStyle, Rectangle};
use embedded_graphics_framebuf::FrameBuf;
use embedded_hal_bus::spi::ExclusiveDevice;
use embedded_io_async::Write;

use mipidsi::interface::SpiInterface;
use mipidsi::options::{ColorInversion, Orientation, Rotation};
use mipidsi::{models::ST7789, Builder};

use eye_anim::{EyeConfig, EyeFrame, EyeRender, Eyes};
use esp_println::println;

esp_bootloader_esp_idf::esp_app_desc!();

// WiFi credentials, baked in at build time. The Makefile's `lights` target
// sources wifi.env so these are set; if you build without them it just won't
// connect (the animation still runs).
const SSID: &str = match option_env!("WIFI_SSID") {
    Some(s) => s,
    None => "",
};
const PASSWORD: &str = match option_env!("WIFI_PASSWORD") {
    Some(s) => s,
    None => "",
};

// How long one `/toggle-lights` hit makes him struggle.
const WORK_BURST_MS: u32 = 4000;

// DHCP hostname (option 12). Many routers register this in their local DNS, so
// you can often reach the device by name (e.g. http://esp-eyes/) instead of a
// changing IP. For a guaranteed `<name>.local` you'd add an mDNS responder.
const HOSTNAME: &str = "esp-eyes";

// Shared trigger: the deadline (embassy-time millis) until which he should be
// "working". The HTTP task writes it; the animation loop reads it. AtomicU32 is
// natively supported on the Xtensa core (uptime millis fit in u32 for ~49 days).
static WORK_UNTIL_MS: AtomicU32 = AtomicU32::new(0);

// ---- Adafruit ESP32-S3 TFT Feather pinout (see eyes.rs) --------------------
const PANEL_OFFSET: (u16, u16) = (52, 40);
const ROTATION: Rotation = Rotation::Deg270;
const CELL_W: usize = EyeConfig::feather_tft().eye_w as usize + 16;
const CELL_H: usize = EyeConfig::feather_tft().eye_h as usize + 20;

/// Allocate a `'static` value without nightly `#![feature]`s.
macro_rules! mk_static {
    ($t:ty, $val:expr) => {{
        static CELL: static_cell::StaticCell<$t> = static_cell::StaticCell::new();
        CELL.uninit().write($val)
    }};
}

#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    esp_println::logger::init_logger_from_env();

    let p = esp_hal::init(esp_hal::Config::default().with_cpu_clock(CpuClock::max()));

    // WiFi needs a heap.
    esp_alloc::heap_allocator!(size: 96 * 1024);

    // Start the esp-rtos scheduler (this also brings up the radio subsystem).
    let timg0 = TimerGroup::new(p.TIMG0);
    let sw_int = SoftwareInterruptControl::new(p.SW_INTERRUPT);
    esp_rtos::start(timg0.timer0, sw_int.software_interrupt0);

    // ---- WiFi station ------------------------------------------------------
    let sta_config = WifiConfig::Station(
        StationConfig::default()
            .with_ssid(SSID)
            .with_password(PASSWORD.into()),
    );
    let (controller, interfaces) = esp_radio::wifi::new(
        p.WIFI,
        ControllerConfig::default().with_initial_config(sta_config),
    )
    .unwrap();

    // ---- embassy-net stack with DHCP --------------------------------------
    let rng = Rng::new();
    let seed = ((rng.random() as u64) << 32) | rng.random() as u64;
    let mut dhcp = embassy_net::DhcpConfig::default();
    dhcp.hostname = Some(heapless::String::try_from(HOSTNAME).unwrap());
    let (stack, runner) = embassy_net::new(
        interfaces.station,
        embassy_net::Config::dhcpv4(dhcp),
        mk_static!(StackResources<4>, StackResources::<4>::new()),
        seed,
    );

    // In embassy-executor 0.10 the task fn returns a `Result<SpawnToken, _>` and
    // `spawn` itself returns `()`, so we unwrap the token before spawning.
    spawner.spawn(connection(controller).unwrap());
    spawner.spawn(net_task(runner).unwrap());
    spawner.spawn(report_ip(stack).unwrap());
    spawner.spawn(http_server(stack).unwrap());

    // ---- Display + animation (runs forever in main) ------------------------
    let mut delay = Delay::new();
    let _panel_power = Output::new(p.GPIO21, Level::High, OutputConfig::default());
    let _backlight = Output::new(p.GPIO45, Level::High, OutputConfig::default());

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
    let mut cell = [Rgb565::BLACK; CELL_W * CELL_H];
    let mut last_frame: Option<EyeFrame> = None;

    loop {
        let now_ms = Instant::now().as_millis();

        // Struggle while we're inside an endpoint-triggered burst.
        let working = (now_ms as u32) < WORK_UNTIL_MS.load(Ordering::Relaxed);
        eyes.set_working(working);

        let frame = eyes.update(now_ms);
        if last_frame != Some(frame) {
            render_eye(&mut display, &mut cell, cfg.left_center(), frame.left);
            render_eye(&mut display, &mut cell, cfg.right_center(), frame.right);
            last_frame = Some(frame);
        }

        // Yield ~4ms so the network tasks run between animation ticks.
        Timer::after(Duration::from_millis(4)).await;
    }
}

/// Keep the WiFi connection up, reconnecting if it drops.
#[embassy_executor::task]
async fn connection(mut controller: WifiController<'static>) {
    println!("wifi: connecting to '{SSID}'");
    loop {
        match controller.connect_async().await {
            Ok(_) => {
                println!("wifi: connected");
                controller.wait_for_disconnect_async().await.ok();
                println!("wifi: disconnected, retrying");
            }
            Err(e) => {
                println!("wifi: connect failed: {e:?}");
                Timer::after(Duration::from_millis(2000)).await;
            }
        }
    }
}

/// Drive the embassy-net stack.
#[embassy_executor::task]
async fn net_task(mut runner: Runner<'static, Interface<'static>>) {
    runner.run().await
}

/// Print the IP once DHCP succeeds, so we know where to curl.
#[embassy_executor::task]
async fn report_ip(stack: embassy_net::Stack<'static>) {
    stack.wait_config_up().await;
    if let Some(cfg) = stack.config_v4() {
        let ip = cfg.address.address();
        println!("net: ready! visit  http://{ip}/toggle-lights  (or try http://{HOSTNAME}/toggle-lights)");
    }
}

/// Minimal HTTP/1.0 server: any request to `/toggle-lights` starts a struggle
/// burst; everything else just gets a short help page.
#[embassy_executor::task]
async fn http_server(stack: embassy_net::Stack<'static>) {
    let mut rx = [0u8; 1536];
    let mut tx = [0u8; 1536];
    loop {
        let mut socket = TcpSocket::new(stack, &mut rx, &mut tx);
        socket.set_timeout(Some(Duration::from_secs(10)));

        if socket
            .accept(IpListenEndpoint { addr: None, port: 80 })
            .await
            .is_err()
        {
            continue;
        }

        // Read the request (just enough to see the request line).
        let mut buf = [0u8; 512];
        let mut n = 0;
        loop {
            match socket.read(&mut buf[n..]).await {
                Ok(0) => break,
                Ok(len) => {
                    n += len;
                    let seen = &buf[..n];
                    // Stop once we've read the end of the headers (or filled buf).
                    if n >= buf.len()
                        || seen.windows(4).any(|w| w == b"\r\n\r\n")
                    {
                        break;
                    }
                }
                Err(_) => break,
            }
        }

        let req = core::str::from_utf8(&buf[..n]).unwrap_or("");
        let triggered = req.contains("/toggle-lights");

        let body: &[u8] = if triggered {
            let until = (Instant::now().as_millis() as u32).wrapping_add(WORK_BURST_MS);
            WORK_UNTIL_MS.store(until, Ordering::Relaxed);
            println!("http: /toggle-lights -> struggling for {WORK_BURST_MS}ms");
            b"toggled: struggling for a few seconds\n"
        } else {
            b"esp32 eyes. GET /toggle-lights to trigger the struggle.\n"
        };

        let mut header = [0u8; 96];
        let header = http_header(&mut header, body.len());
        let _ = socket.write_all(header).await;
        let _ = socket.write_all(body).await;
        let _ = socket.flush().await;
        Timer::after(Duration::from_millis(50)).await;
        socket.close();
        Timer::after(Duration::from_millis(50)).await;
        socket.abort();
    }
}

/// Build a tiny `HTTP/1.0 200` header with the given content length into `buf`.
fn http_header(buf: &mut [u8; 96], content_len: usize) -> &[u8] {
    use core::fmt::Write as _;
    struct W<'a> {
        buf: &'a mut [u8; 96],
        n: usize,
    }
    impl core::fmt::Write for W<'_> {
        fn write_str(&mut self, s: &str) -> core::fmt::Result {
            let b = s.as_bytes();
            if self.n + b.len() > self.buf.len() {
                return Err(core::fmt::Error);
            }
            self.buf[self.n..self.n + b.len()].copy_from_slice(b);
            self.n += b.len();
            Ok(())
        }
    }
    let mut w = W { buf, n: 0 };
    let _ = write!(
        w,
        "HTTP/1.0 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {content_len}\r\nConnection: close\r\n\r\n"
    );
    let n = w.n;
    &buf[..n]
}

/// Render one eye into the cell framebuffer and blit it (flicker-free).
fn render_eye<D>(display: &mut D, cell: &mut [Rgb565; CELL_W * CELL_H], nominal: (i32, i32), eye: EyeRender)
where
    D: DrawTarget<Color = Rgb565>,
{
    let white = PrimitiveStyle::with_fill(Rgb565::WHITE);
    let cell_x = nominal.0 - CELL_W as i32 / 2;
    let cell_y = nominal.1 - CELL_H as i32 / 2;

    {
        let mut fb = FrameBuf::new(&mut *cell, CELL_W, CELL_H);
        let _ = fb.clear(Rgb565::BLACK);
        let ex = (eye.cx - eye.w / 2) - cell_x;
        let ey = (eye.cy - eye.h / 2) - cell_y;
        let _ = Ellipse::new(Point::new(ex, ey), Size::new(eye.w as u32, eye.h as u32))
            .into_styled(white)
            .draw(&mut fb);
    }

    let area = Rectangle::new(
        Point::new(cell_x, cell_y),
        Size::new(CELL_W as u32, CELL_H as u32),
    );
    let _ = display.fill_contiguous(&area, cell.iter().copied());
}
