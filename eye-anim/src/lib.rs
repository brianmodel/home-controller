//! Hardware-independent logic for the ESP32-S3 eyes.
//!
//! Everything here is plain `no_std` math with **no** hardware dependencies, so
//! it can be unit-tested on the host with a normal `cargo test`. The firmware
//! binary pulls [`Eyes`] in, asks it for the current [`EyeFrame`] each tick, and
//! draws the two ellipses to the ST7789.
//!
//! Behaviour:
//! - **Idle** (resting): the eyes blink occasionally, with rare double-blinks.
//!   Ported from github.com/dmtrKovalenko/esp32-smooth-eye-blinking.
//! - **Working** (struggle): toggled on via [`Eyes::set_working`]. The eyes
//!   squeeze shut hard and tremble, clenching tighter in pulses — like he's
//!   straining to do something.
//! - **Transitions** between the two are always smoothly eased; you can toggle
//!   at any moment and the motion is continuous from wherever it was.
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

/// A smooth `0 -> 1 -> 0` triangle wave. `cycles` is elapsed time measured in
/// whole periods (must be >= 0). Used to pulse the clench/tremble while working.
fn pulse(cycles: f32) -> f32 {
    let frac = cycles - (cycles as i64 as f32); // fractional part, 0..1 (cycles >= 0)
    let tri = if frac < 0.5 { frac * 2.0 } else { 2.0 - frac * 2.0 };
    ease_in_out(tri)
}

/// "Relief" envelope over `t` in `[0, 1]`: a quick rise to 1 then a slow decay
/// back to 0 — eyes pop wide with relief, then settle. Returns `0..1`.
fn relief_curve(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    const RISE: f32 = 0.22;
    if t < RISE {
        ease_in_out(t / RISE) // snap wide open
    } else {
        1.0 - ease_in_out((t - RISE) / (1.0 - RISE)) // slowly relax to normal
    }
}

/// Tiny deterministic PRNG (xorshift32). `no_std`, no allocation, fully testable.
#[derive(Clone)]
pub struct Rng(u32);

impl Rng {
    pub fn new(seed: u32) -> Self {
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

    /// A signed jitter in `[-amp, amp]`.
    fn jitter(&mut self, amp: i32) -> i32 {
        if amp <= 0 {
            return 0;
        }
        self.range_u32(0, (2 * amp + 1) as u32) as i32 - amp
    }
}

/// Static geometry of the two eyes, in pixels.
#[derive(Clone, Copy)]
pub struct EyeConfig {
    pub screen_w: i32,
    pub screen_h: i32,
    /// Ellipse width when relaxed (only height animates during a blink).
    pub eye_w: i32,
    /// Ellipse height when fully open.
    pub eye_h: i32,
    /// Distance between the two eye centers.
    pub spacing: i32,
    /// Height at full squeeze (never fully zero, so a line stays).
    pub min_h: i32,
}

impl EyeConfig {
    /// Matches the reference on the 240x135 Feather TFT.
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

    /// Center (x, y) of the left eye when at rest.
    pub fn left_center(&self) -> (i32, i32) {
        (self.screen_w / 2 - self.spacing / 2, self.screen_h / 2)
    }

    /// Center (x, y) of the right eye when at rest.
    pub fn right_center(&self) -> (i32, i32) {
        (self.screen_w / 2 + self.spacing / 2, self.screen_h / 2)
    }
}

/// One eye to draw: an ellipse of size `w x h` centered at `(cx, cy)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EyeRender {
    pub cx: i32,
    pub cy: i32,
    pub w: i32,
    pub h: i32,
}

/// What to draw this tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EyeFrame {
    pub left: EyeRender,
    pub right: EyeRender,
}

// ---- Idle blink timing -----------------------------------------------------
const CLOSE_MS: u32 = 90;
const OPEN_MS: u32 = 160;
const BLINK_MS: u32 = CLOSE_MS + OPEN_MS;
const OPEN_MIN_MS: u32 = 1000;
const OPEN_MAX_MS: u32 = 4000;
const DOUBLE_BLINK_GAP_MS: u32 = 180;
const DOUBLE_BLINK_NUM: u32 = 1; // 1/10 = 10%
const DOUBLE_BLINK_DEN: u32 = 10;

// ---- Work / struggle tuning ------------------------------------------------
/// How long the smooth ease into / out of the working state takes.
const ENTER_MS: f32 = 450.0;
const EXIT_MS: f32 = 500.0;
/// Openness while working oscillates between these (tight squeeze .. slightly less).
const WORK_OPEN_LOOSE: f32 = 0.12;
const WORK_OPEN_TIGHT: f32 = 0.02;
/// Period of the clench pulse while working.
const CLENCH_PERIOD_MS: f32 = 620.0;
/// Max tremble amplitude (pixels) at peak clench.
const TREMBLE_MAX: i32 = 3;
/// Eyes narrow slightly at peak clench (wince).
const WORK_WSCALE_TIGHT: f32 = 0.80;
/// Relief beat after releasing the squeeze: how long, and how wide the eyes pop
/// (1.0 = normal size, 1.25 = 25% bigger at the peak of relief).
const RELIEF_MS: f32 = 850.0;
const RELIEF_SCALE: f32 = 0.25;

#[derive(Clone, Copy)]
enum Phase {
    Idle,
    EnterWork { start_ms: u64, from_open: f32 },
    Working { start_ms: u64 },
    ExitWork { start_ms: u64, from_open: f32 },
    /// After un-squeezing: eyes wide with relief, settling back to normal.
    Relief { start_ms: u64 },
}

/// The eye controller. Drive it by calling [`Eyes::update`] every tick with a
/// monotonically increasing millisecond timestamp, and toggle the working state
/// with [`Eyes::set_working`].
pub struct Eyes {
    cfg: EyeConfig,
    rng: Rng,
    phase: Phase,
    desired_working: bool,
    /// Last rendered base openness, so transitions start exactly where we are.
    cur_open: f32,

    // Idle blink scheduling.
    blinking: bool,
    blink_start_ms: u64,
    next_blink_at_ms: u64,
    pending_double: bool,
}

impl Eyes {
    pub fn new(cfg: EyeConfig, seed: u32, now_ms: u64) -> Self {
        let mut rng = Rng::new(seed);
        let first = now_ms + rng.range_u32(OPEN_MIN_MS, OPEN_MAX_MS) as u64;
        Self {
            cfg,
            rng,
            phase: Phase::Idle,
            desired_working: false,
            cur_open: 1.0,
            blinking: false,
            blink_start_ms: 0,
            next_blink_at_ms: first,
            pending_double: false,
        }
    }

    pub fn config(&self) -> EyeConfig {
        self.cfg
    }

    /// Toggle/set the working (struggle) state. Safe to call at any time; the
    /// transition eases smoothly from wherever the eyes currently are.
    pub fn set_working(&mut self, working: bool) {
        self.desired_working = working;
    }

    pub fn is_working(&self) -> bool {
        self.desired_working
    }

    /// Openness in `[0, 1]` of a blink that started `elapsed` ms ago. Closes over
    /// `CLOSE_MS`, then opens over `OPEN_MS`, both eased.
    fn blink_openness(elapsed_ms: u32) -> f32 {
        if elapsed_ms < CLOSE_MS {
            let t = elapsed_ms as f32 / CLOSE_MS as f32;
            1.0 - ease_in_out(t)
        } else {
            let t = (elapsed_ms - CLOSE_MS) as f32 / OPEN_MS as f32;
            ease_in_out(t)
        }
    }

    fn schedule_next_blink(&mut self, now_ms: u64) {
        self.blinking = false;
        if self.pending_double {
            self.pending_double = false;
            self.next_blink_at_ms = now_ms + DOUBLE_BLINK_GAP_MS as u64;
        } else {
            self.pending_double = self.rng.chance(DOUBLE_BLINK_NUM, DOUBLE_BLINK_DEN);
            let open = self.rng.range_u32(OPEN_MIN_MS, OPEN_MAX_MS);
            self.next_blink_at_ms = now_ms + open as u64;
        }
    }

    /// Idle behaviour: schedule + run blinks, return current openness.
    fn idle_openness(&mut self, now_ms: u64) -> f32 {
        if !self.blinking && now_ms >= self.next_blink_at_ms {
            self.blinking = true;
            self.blink_start_ms = now_ms;
        }
        if self.blinking {
            let elapsed = (now_ms - self.blink_start_ms) as u32;
            if elapsed >= BLINK_MS {
                self.schedule_next_blink(now_ms);
                1.0
            } else {
                Self::blink_openness(elapsed)
            }
        } else {
            1.0
        }
    }

    /// Advance the animation and return what to draw now.
    pub fn update(&mut self, now_ms: u64) -> EyeFrame {
        // 1. Begin a transition if the desired state no longer matches the phase.
        //    `cur_open` makes every transition start exactly where we are.
        match self.phase {
            Phase::Idle if self.desired_working => {
                self.phase = Phase::EnterWork {
                    start_ms: now_ms,
                    from_open: self.cur_open,
                };
            }
            Phase::Working { .. } if !self.desired_working => {
                self.phase = Phase::ExitWork {
                    start_ms: now_ms,
                    from_open: self.cur_open,
                };
            }
            Phase::EnterWork { .. } if !self.desired_working => {
                self.phase = Phase::ExitWork {
                    start_ms: now_ms,
                    from_open: self.cur_open,
                };
            }
            Phase::ExitWork { .. } if self.desired_working => {
                self.phase = Phase::EnterWork {
                    start_ms: now_ms,
                    from_open: self.cur_open,
                };
            }
            Phase::Relief { .. } if self.desired_working => {
                self.phase = Phase::EnterWork {
                    start_ms: now_ms,
                    from_open: self.cur_open,
                };
            }
            _ => {}
        }

        // 2. Produce base openness, width scale, tremble amplitude, and an overall
        //    size scale (used for the wide-eyed relief beat).
        let (base_open, wscale, tremble, scale) = match self.phase {
            Phase::Idle => (self.idle_openness(now_ms), 1.0, 0.0, 1.0),

            Phase::EnterWork { start_ms, from_open } => {
                let e = ease_in_out((now_ms - start_ms) as f32 / ENTER_MS);
                let open = lerp(from_open, WORK_OPEN_LOOSE, e);
                if (now_ms - start_ms) as f32 >= ENTER_MS {
                    self.phase = Phase::Working { start_ms: now_ms };
                }
                (open, 1.0, e, 1.0) // tremble ramps 0 -> 1px
            }

            Phase::Working { start_ms } => {
                let p = pulse((now_ms - start_ms) as f32 / CLENCH_PERIOD_MS);
                let open = lerp(WORK_OPEN_LOOSE, WORK_OPEN_TIGHT, p);
                let wscale = lerp(1.0, WORK_WSCALE_TIGHT, p);
                let tremble = lerp(1.0, TREMBLE_MAX as f32, p);
                (open, wscale, tremble, 1.0)
            }

            Phase::ExitWork { start_ms, from_open } => {
                let e = ease_in_out((now_ms - start_ms) as f32 / EXIT_MS);
                let open = lerp(from_open, 1.0, e);
                if (now_ms - start_ms) as f32 >= EXIT_MS {
                    // Un-squeezed → now show relief before normal blinking.
                    self.phase = Phase::Relief { start_ms: now_ms };
                }
                let tremble = lerp(TREMBLE_MAX as f32, 0.0, e);
                (open, lerp(WORK_WSCALE_TIGHT, 1.0, e), tremble, 1.0)
            }

            Phase::Relief { start_ms } => {
                let t = (now_ms - start_ms) as f32 / RELIEF_MS;
                let scale = 1.0 + RELIEF_SCALE * relief_curve(t);
                if (now_ms - start_ms) as f32 >= RELIEF_MS {
                    self.phase = Phase::Idle;
                    self.blinking = false;
                    self.pending_double = false;
                    self.next_blink_at_ms =
                        now_ms + self.rng.range_u32(OPEN_MIN_MS, OPEN_MAX_MS) as u64;
                }
                (1.0, 1.0, 0.0, scale)
            }
        };

        self.cur_open = base_open;

        // 3. Convert to pixels, adding an independent tremble per eye.
        let amp = tremble as i32;
        let left = self.render(base_open, wscale, scale, self.cfg.left_center(), amp);
        let right = self.render(base_open, wscale, scale, self.cfg.right_center(), amp);
        EyeFrame { left, right }
    }

    fn render(&mut self, open: f32, wscale: f32, scale: f32, center: (i32, i32), amp: i32) -> EyeRender {
        let h = (lerp(self.cfg.min_h as f32, self.cfg.eye_h as f32, open) * scale + 0.5) as i32;
        let w = (self.cfg.eye_w as f32 * wscale * scale + 0.5) as i32;
        EyeRender {
            cx: center.0 + self.rng.jitter(amp),
            cy: center.1 + self.rng.jitter(amp),
            w: w.max(1),
            h: h.max(1),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CFG: EyeConfig = EyeConfig::feather_tft();

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
    fn pulse_is_zero_at_cycle_boundaries_and_one_at_half() {
        assert!(pulse(0.0) < 1e-6);
        assert!(pulse(1.0) < 1e-6);
        assert!(pulse(2.0) < 1e-6);
        assert!((pulse(0.5) - 1.0).abs() < 1e-6);
        assert!((pulse(1.5) - 1.0).abs() < 1e-6);
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
    fn jitter_stays_in_bounds() {
        let mut r = Rng::new(99);
        for _ in 0..10_000 {
            let j = r.jitter(3);
            assert!((-3..=3).contains(&j));
        }
        assert_eq!(Rng::new(1).jitter(0), 0);
    }

    #[test]
    fn blink_curve_shape() {
        assert!((Eyes::blink_openness(0) - 1.0).abs() < 1e-6);
        assert!((Eyes::blink_openness(BLINK_MS) - 1.0).abs() < 1e-6);
        let closed = Eyes::blink_openness(CLOSE_MS);
        assert!(closed < 0.05, "should be ~0 at full close, got {closed}");
    }

    #[test]
    fn eyes_start_open_and_centered() {
        let mut eyes = Eyes::new(CFG, 7, 0);
        let f = eyes.update(0);
        assert_eq!(f.left.h, CFG.eye_h);
        assert_eq!(f.right.h, CFG.eye_h);
        assert_eq!(f.left.w, CFG.eye_w);
        assert_eq!((f.left.cx, f.left.cy), CFG.left_center());
        assert_eq!((f.right.cx, f.right.cy), CFG.right_center());
    }

    #[test]
    fn idle_blink_closes_then_reopens() {
        let mut eyes = Eyes::new(CFG, 7, 0);
        let start = eyes.next_blink_at_ms;
        eyes.update(start);
        let mid = eyes.update(start + CLOSE_MS as u64);
        assert!(mid.left.h <= CFG.min_h + 4, "got {}", mid.left.h);
        let after = eyes.update(start + BLINK_MS as u64 + 1);
        assert_eq!(after.left.h, CFG.eye_h);
    }

    /// Step the controller forward in 4ms ticks for `ms` milliseconds, starting
    /// at `t0`, returning the last frame and the new time.
    fn run(eyes: &mut Eyes, t0: u64, ms: u64) -> (EyeFrame, u64) {
        let mut t = t0;
        let mut f = eyes.update(t);
        while t < t0 + ms {
            t += 4;
            f = eyes.update(t);
        }
        (f, t)
    }

    #[test]
    fn working_squeezes_eyes_hard() {
        let mut eyes = Eyes::new(CFG, 7, 0);
        eyes.set_working(true);
        // After the enter transition completes, eyes are squeezed near-shut.
        let (_f, t) = run(&mut eyes, 0, 1000);
        // Sample across a clench period; height must stay very small throughout.
        let mut max_h = 0;
        let mut tt = t;
        for _ in 0..200 {
            tt += 4;
            let f = eyes.update(tt);
            max_h = max_h.max(f.left.h);
        }
        assert!(
            max_h <= CFG.min_h + 8,
            "while working, eyes should stay squeezed; max height was {max_h}"
        );
    }

    #[test]
    fn working_trembles() {
        let mut eyes = Eyes::new(CFG, 7, 0);
        eyes.set_working(true);
        let (_f, mut t) = run(&mut eyes, 0, 1200);
        // Over a window, the eye center should move around (tremble), staying
        // within a few px of the resting center.
        let (rcx, rcy) = CFG.left_center();
        let mut moved = false;
        for _ in 0..200 {
            t += 4;
            let f = eyes.update(t);
            if f.left.cx != rcx || f.left.cy != rcy {
                moved = true;
            }
            assert!((f.left.cx - rcx).abs() <= TREMBLE_MAX);
            assert!((f.left.cy - rcy).abs() <= TREMBLE_MAX);
        }
        assert!(moved, "working eyes should tremble");
    }

    #[test]
    fn toggling_off_returns_to_open_then_blinking() {
        let mut eyes = Eyes::new(CFG, 7, 0);
        eyes.set_working(true);
        let (_f, t) = run(&mut eyes, 0, 1500); // enter + work a bit
        eyes.set_working(false);
        let (f, _t) = run(&mut eyes, t, 1500); // exit transition completes
        // Back to fully open (idle, between blinks).
        assert_eq!(f.left.h, CFG.eye_h);
        assert_eq!(f.left.w, CFG.eye_w);
    }

    #[test]
    fn relief_pops_eyes_wide_then_settles() {
        let mut eyes = Eyes::new(CFG, 7, 0);
        eyes.set_working(true);
        let (_f, t) = run(&mut eyes, 0, 1500); // enter + work
        eyes.set_working(false);
        // Recovery: exit squeeze (500ms) then relief (850ms). Somewhere in there
        // the eyes should open *wider* than normal, then settle back.
        let mut saw_wide = false;
        let mut tt = t;
        let mut settled_h = 0;
        while tt < t + 1400 {
            tt += 4;
            let f = eyes.update(tt);
            if f.left.h > CFG.eye_h {
                saw_wide = true;
            }
            settled_h = f.left.h;
        }
        assert!(saw_wide, "expected a wide-eyed relief beat");
        // 50ms after the relief finishes (before the next blink) it's normal.
        assert_eq!(settled_h, CFG.eye_h);
    }

    #[test]
    fn transition_is_smooth_no_height_jumps() {
        // The per-tick change in eye height should never be large — proving the
        // enter/work/exit transitions are continuous (no snapping).
        let mut eyes = Eyes::new(CFG, 7, 0);
        let mut t = 0u64;
        let mut prev = eyes.update(t).left.h;
        let mut max_jump = 0;
        // idle a bit, then work, then back.
        let script = [(2000u64, false), (3000, true), (3000, false)];
        for (dur, working) in script {
            eyes.set_working(working);
            let end = t + dur;
            while t < end {
                t += 4;
                let h = eyes.update(t).left.h;
                max_jump = max_jump.max((h - prev).abs());
                prev = h;
            }
        }
        // A normal blink closes ~48px over 90ms (~24 ticks) → ~2px/tick. The
        // work transitions are slower. Allow generous headroom but catch snaps.
        assert!(max_jump <= 6, "height jumped {max_jump}px in one tick");
    }

    #[test]
    fn geometry_centers_are_symmetric() {
        assert_eq!(CFG.left_center().1, CFG.right_center().1);
        assert_eq!(CFG.right_center().0 - CFG.left_center().0, CFG.spacing);
        assert_eq!(CFG.left_center().0 + CFG.right_center().0, CFG.screen_w);
    }
}
