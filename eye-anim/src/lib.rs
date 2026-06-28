//! Hardware-independent logic for the ESP32-S3 eyes.
//!
//! Everything here is plain `no_std` math with **no** hardware dependencies, so
//! it can be unit-tested on the host with a normal `cargo test`. The firmware
//! binary pulls [`Eyes`] in, asks it for the current [`EyeFrame`] each tick, and
//! draws the two ellipses to the ST7789.
//!
//! Ported from the behaviour of the C++ reference
//! (github.com/dmtrKovalenko/esp32-smooth-eye-blinking): two white ellipses that
//! blink by shrinking their height, with randomized idle time and an occasional
//! quick double-blink.
#![cfg_attr(not(test), no_std)]

/// Smooth ease-in-out curve (cubic) mapping `t` in `[0.0, 1.0]` to `[0.0, 1.0]`.
pub fn ease_in_out(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    if t < 0.5 {
        4.0 * t * t * t
    } else {
        let f = 2.0 * t - 2.0;
        0.5 * f * f * f + 1.0
    }
}

/// Linear interpolation from `a` to `b` by `t` in `[0.0, 1.0]`.
pub fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t.clamp(0.0, 1.0)
}

/// Tiny deterministic PRNG (xorshift32). `no_std`, no allocation, and fully
/// testable. On hardware we seed it from the RNG peripheral; in tests we seed it
/// with a fixed value for reproducibility.
#[derive(Clone)]
pub struct Rng(u32);

impl Rng {
    pub fn new(seed: u32) -> Self {
        // Avoid the all-zero state, which xorshift can't escape.
        Rng(if seed == 0 { 0xC0FF_EE00 } else { seed })
    }

    pub fn next_u32(&mut self) -> u32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = x;
        x
    }

    /// Uniform in `[lo, hi)`. `hi` must be > `lo`.
    pub fn range_u32(&mut self, lo: u32, hi: u32) -> u32 {
        lo + self.next_u32() % (hi - lo)
    }

    /// True with probability `numerator / denominator`.
    pub fn chance(&mut self, numerator: u32, denominator: u32) -> bool {
        self.next_u32() % denominator < numerator
    }
}

/// Static geometry of the two eyes, in pixels.
#[derive(Clone, Copy)]
pub struct EyeConfig {
    pub screen_w: i32,
    pub screen_h: i32,
    /// Ellipse width (constant; only the height animates).
    pub eye_w: i32,
    /// Ellipse height when fully open.
    pub eye_h: i32,
    /// Distance between the two eye centers.
    pub spacing: i32,
    /// Height at the bottom of a blink (never fully zero, so a line stays).
    pub min_h: i32,
}

impl EyeConfig {
    /// Matches the C++ reference on the 240x135 Feather TFT.
    pub const fn feather_tft() -> Self {
        Self {
            screen_w: 240,
            screen_h: 135,
            eye_w: 30,
            eye_h: 50,
            spacing: 80,
            min_h: 2,
        }
    }

    /// Center (x, y) of the left eye.
    pub fn left_center(&self) -> (i32, i32) {
        (self.screen_w / 2 - self.spacing / 2, self.screen_h / 2)
    }

    /// Center (x, y) of the right eye.
    pub fn right_center(&self) -> (i32, i32) {
        (self.screen_w / 2 + self.spacing / 2, self.screen_h / 2)
    }
}

/// What to draw this tick: the current height of each eye (width is constant).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EyeFrame {
    pub left_h: i32,
    pub right_h: i32,
}

/// Timing constants, in milliseconds. A natural eyelid closes a bit faster than
/// it opens; total ~250ms reads as a deliberate, smooth blink (the 80ms of the
/// reference looks like a snap on this panel).
const CLOSE_MS: u32 = 90;
const OPEN_MS: u32 = 160;
const BLINK_MS: u32 = CLOSE_MS + OPEN_MS;
const OPEN_MIN_MS: u32 = 1000;
const OPEN_MAX_MS: u32 = 4000;
/// A double-blink fires its second blink shortly after the first finishes.
const DOUBLE_BLINK_GAP_MS: u32 = 180;
const DOUBLE_BLINK_NUM: u32 = 1; // 1/10 = 10%
const DOUBLE_BLINK_DEN: u32 = 10;

/// The blink state machine. Drive it by calling [`Eyes::update`] every tick with
/// a monotonically increasing millisecond timestamp.
pub struct Eyes {
    cfg: EyeConfig,
    rng: Rng,
    blinking: bool,
    blink_start_ms: u64,
    next_blink_at_ms: u64,
    /// If set, the *next* scheduled blink is a quick second blink.
    pending_double: bool,
}

impl Eyes {
    pub fn new(cfg: EyeConfig, seed: u32, now_ms: u64) -> Self {
        let mut rng = Rng::new(seed);
        let first = now_ms + rng.range_u32(OPEN_MIN_MS, OPEN_MAX_MS) as u64;
        Self {
            cfg,
            rng,
            blinking: false,
            blink_start_ms: 0,
            next_blink_at_ms: first,
            pending_double: false,
        }
    }

    pub fn config(&self) -> EyeConfig {
        self.cfg
    }

    /// Eye openness in `[0.0, 1.0]` for a blink that started `elapsed` ms ago.
    /// Closes over `CLOSE_MS`, then opens over `OPEN_MS`, both with a smooth
    /// ease-in-out so the eyelid accelerates and decelerates naturally.
    fn openness(elapsed_ms: u32) -> f32 {
        if elapsed_ms < CLOSE_MS {
            let t = elapsed_ms as f32 / CLOSE_MS as f32; // 0 -> 1 while closing
            1.0 - ease_in_out(t)
        } else {
            let t = (elapsed_ms - CLOSE_MS) as f32 / OPEN_MS as f32; // 0 -> 1 while opening
            ease_in_out(t)
        }
    }

    /// Advance the animation and return what to draw now.
    pub fn update(&mut self, now_ms: u64) -> EyeFrame {
        if !self.blinking && now_ms >= self.next_blink_at_ms {
            self.blinking = true;
            self.blink_start_ms = now_ms;
        }

        let openness = if self.blinking {
            let elapsed = (now_ms - self.blink_start_ms) as u32;
            if elapsed >= BLINK_MS {
                // Blink finished — schedule the next one.
                self.blinking = false;
                if self.pending_double {
                    self.pending_double = false;
                    self.next_blink_at_ms = now_ms + DOUBLE_BLINK_GAP_MS as u64;
                } else {
                    self.pending_double =
                        self.rng.chance(DOUBLE_BLINK_NUM, DOUBLE_BLINK_DEN);
                    let open = self.rng.range_u32(OPEN_MIN_MS, OPEN_MAX_MS);
                    self.next_blink_at_ms = now_ms + open as u64;
                }
                1.0
            } else {
                Self::openness(elapsed)
            }
        } else {
            1.0
        };

        let h = lerp(self.cfg.min_h as f32, self.cfg.eye_h as f32, openness);
        // `f32::round` lives in std; do it by hand so this stays `no_std`-clean.
        // `h` is always >= 0 here, so +0.5 then truncate rounds to nearest.
        let h = (h + 0.5) as i32;
        EyeFrame {
            left_h: h,
            right_h: h,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ease_endpoints_are_pinned() {
        assert_eq!(ease_in_out(0.0), 0.0);
        assert_eq!(ease_in_out(1.0), 1.0);
    }

    #[test]
    fn lerp_basic() {
        assert_eq!(lerp(0.0, 10.0, 0.5), 5.0);
        assert_eq!(lerp(2.0, 4.0, 0.0), 2.0);
        assert_eq!(lerp(2.0, 4.0, 1.0), 4.0);
    }

    #[test]
    fn rng_is_deterministic_and_nonzero() {
        let mut a = Rng::new(1);
        let mut b = Rng::new(1);
        for _ in 0..1000 {
            let v = a.next_u32();
            assert_eq!(v, b.next_u32());
            assert_ne!(v, 0);
        }
    }

    #[test]
    fn rng_range_is_in_bounds() {
        let mut r = Rng::new(42);
        for _ in 0..10_000 {
            let v = r.range_u32(1000, 4000);
            assert!((1000..4000).contains(&v));
        }
    }

    #[test]
    fn openness_curve_shape() {
        // Fully open at the very start and very end of the blink window.
        assert!((Eyes::openness(0) - 1.0).abs() < 1e-6);
        assert!((Eyes::openness(BLINK_MS) - 1.0).abs() < 1e-6);
        // Fully closed at the close/open boundary.
        let closed = Eyes::openness(CLOSE_MS);
        assert!(closed < 0.05, "should be ~0 at full close, got {closed}");
        // Partway into the close, the eye is partly shut (between open and closed).
        let mid_close = Eyes::openness(CLOSE_MS / 2);
        assert!(mid_close > closed && mid_close < 1.0);
    }

    #[test]
    fn eyes_start_open() {
        let mut eyes = Eyes::new(EyeConfig::feather_tft(), 7, 0);
        let f = eyes.update(0);
        assert_eq!(f.left_h, 50);
        assert_eq!(f.right_h, 50);
    }

    #[test]
    fn eyes_blink_closes_then_reopens() {
        let cfg = EyeConfig::feather_tft();
        let mut eyes = Eyes::new(cfg, 7, 0);
        let start = eyes.next_blink_at_ms;
        // Trigger the blink exactly on schedule (the real loop ticks every few ms).
        eyes.update(start);
        // At full close the eye is near its minimum height.
        let mid = eyes.update(start + CLOSE_MS as u64);
        assert!(mid.left_h <= cfg.min_h + 4, "got {}", mid.left_h);
        // After the blink window it is open again.
        let after = eyes.update(start + BLINK_MS as u64 + 1);
        assert_eq!(after.left_h, cfg.eye_h);
    }

    #[test]
    fn geometry_centers_are_symmetric() {
        let cfg = EyeConfig::feather_tft();
        let (lx, ly) = cfg.left_center();
        let (rx, ry) = cfg.right_center();
        assert_eq!(ly, ry);
        assert_eq!(rx - lx, cfg.spacing);
        // Symmetric about screen center.
        assert_eq!(lx + rx, cfg.screen_w);
    }

    #[test]
    fn double_blink_eventually_schedules_a_quick_gap() {
        // Run a long virtual session and confirm at least one pair of blinks
        // occurs only ~300ms apart (the double-blink), proving that path runs.
        let cfg = EyeConfig::feather_tft();
        let mut eyes = Eyes::new(cfg, 12345, 0);
        let mut last_blink_start: Option<u64> = None;
        let mut saw_quick_gap = false;
        let mut was_open = true;
        for t in 0..120_000u64 {
            let f = eyes.update(t);
            let closing = f.left_h < cfg.eye_h;
            if closing && was_open {
                if let Some(prev) = last_blink_start {
                    if t - prev < 500 {
                        saw_quick_gap = true;
                    }
                }
                last_blink_start = Some(t);
            }
            was_open = !closing;
        }
        assert!(saw_quick_gap, "expected at least one double-blink in 120s");
    }
}
