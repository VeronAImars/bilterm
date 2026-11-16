//! TRON light-cycle border clock (`:cycles`). The phase (cells travelled) is derived from elapsed time
//! and the configured speed; it only exists while TRON is on and the speed is above zero, so nothing
//! requests a redraw otherwise. Redraws are requested at most every `max(1000 / speed, 33)` ms.

use std::time::{Duration, Instant};

use super::WindowManager;
use crate::ui::lightcycle::{DEFAULT_SPEED, LightCycleOp};

/// Never redraw the animation faster than this (about 30 per second).
const MIN_FRAME: Duration = Duration::from_millis(33);
/// A stalled loop (suspend, debugger) advances the cycles by at most this much time.
const MAX_STEP_MS: u64 = 250;

/// Animation state: `phase` is `None` while the border is static.
#[derive(Debug, Default)]
pub(crate) struct LightCycleClock {
    last: Option<Instant>,
    /// Cells travelled, times 1000.
    acc_milli: u64,
    phase: Option<u64>,
}

/// Minimum time between animation redraws at `speed` cells per second.
pub fn frame_interval(speed: u8) -> Duration {
    Duration::from_millis(1000 / u64::from(speed.max(1))).max(MIN_FRAME)
}

impl WindowManager {
    /// Cells the light cycles have travelled, `None` while the border is static (TRON off, animation off).
    pub fn lightcycle_phase(&self) -> Option<u64> {
        self.lightcycle.phase
    }

    /// Fixes the animation phase (snapshot tests); the next tick continues from it.
    pub fn set_lightcycle_phase(&mut self, phase: Option<u64>) {
        self.lightcycle.acc_milli = phase.unwrap_or(0).saturating_mul(1000);
        self.lightcycle.phase = phase;
    }

    /// Configured speed in cells per second (`0` = off).
    pub fn lightcycle_speed(&self) -> u8 {
        self.config.appearance.lightcycle_speed.get()
    }

    /// Advances the clock to `now`. Returns `true` when the phase changed (or the animation just
    /// started or stopped) so the caller redraws; `false` costs nothing and requests nothing.
    pub fn tick_lightcycle(&mut self, now: Instant) -> bool {
        let speed = self.lightcycle_speed();
        let on = speed > 0 && crate::ui::design::tron::tron_active(&self.config) && !self.panes.is_empty();
        let c = &mut self.lightcycle;
        if !on {
            c.last = None;
            c.acc_milli = 0;
            return c.phase.take().is_some();
        }
        let Some(prev) = c.last else {
            c.last = Some(now);
            let started = c.phase.is_none();
            c.phase.get_or_insert(0);
            return started;
        };
        let dt = now.saturating_duration_since(prev);
        if dt < frame_interval(speed) {
            return false;
        }
        c.last = Some(now);
        c.acc_milli += (dt.as_millis() as u64).min(MAX_STEP_MS) * u64::from(speed);
        let next = Some(c.acc_milli / 1000);
        let changed = c.phase != next;
        c.phase = next;
        changed
    }

    /// Runs a `:cycles` operation and reports the result in the status line.
    pub(crate) fn apply_lightcycle_op(&mut self, op: LightCycleOp) {
        let cur = self.lightcycle_speed();
        let new = match op {
            LightCycleOp::Show => cur,
            LightCycleOp::On => if cur == 0 { DEFAULT_SPEED } else { cur },
            LightCycleOp::Off => 0,
            LightCycleOp::Speed(n) => n,
        };
        self.config.appearance.lightcycle_speed = crate::ui::lightcycle::CycleSpeed(new);
        let note = if crate::ui::design::tron::tron_active(&self.config) { "" } else { " (animates in the TRON look only: :tron on)" };
        let msg = if new == 0 {
            format!("Light cycles: off{note}")
        } else {
            format!("Light cycles: {new} cells/s{note}")
        };
        self.active_pane_mut().set_status_message(Some(msg));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tron_wm() -> WindowManager {
        let mut wm = WindowManager::new_headless(24, 80);
        crate::ui::design::tron::tron_on(wm.config_mut());
        wm
    }

    #[test]
    fn inactive_when_tron_is_off_or_speed_is_zero() {
        let t0 = Instant::now();
        let mut wm = WindowManager::new_headless(24, 80);
        assert!(!wm.tick_lightcycle(t0));
        assert_eq!(wm.lightcycle_phase(), None);
        let mut wm = tron_wm();
        wm.apply_lightcycle_op(LightCycleOp::Off);
        assert!(!wm.tick_lightcycle(t0));
        assert_eq!(wm.lightcycle_phase(), None);
    }

    #[test]
    fn phase_advances_with_elapsed_time_at_the_configured_speed() {
        let t0 = Instant::now();
        let mut wm = tron_wm();
        wm.apply_lightcycle_op(LightCycleOp::Speed(10));
        assert!(wm.tick_lightcycle(t0), "start requests the first frame");
        assert_eq!(wm.lightcycle_phase(), Some(0));
        assert!(!wm.tick_lightcycle(t0 + Duration::from_millis(16)), "between frames: no redraw");
        assert!(wm.tick_lightcycle(t0 + Duration::from_millis(100)));
        assert_eq!(wm.lightcycle_phase(), Some(1));
        for ms in (200..=1000).step_by(100) {
            assert!(wm.tick_lightcycle(t0 + Duration::from_millis(ms)));
        }
        assert_eq!(wm.lightcycle_phase(), Some(10));
        wm.apply_lightcycle_op(LightCycleOp::Speed(20));
        assert!(wm.tick_lightcycle(t0 + Duration::from_millis(1100)));
        assert_eq!(wm.lightcycle_phase(), Some(12), "faster speed keeps the position and moves quicker");
    }

    #[test]
    fn turning_tron_off_stops_the_animation_with_one_last_redraw() {
        let t0 = Instant::now();
        let mut wm = tron_wm();
        assert!(wm.tick_lightcycle(t0));
        crate::ui::design::tron::tron_off(wm.config_mut());
        assert!(wm.tick_lightcycle(t0 + Duration::from_millis(50)), "static border redraw");
        assert_eq!(wm.lightcycle_phase(), None);
        assert!(!wm.tick_lightcycle(t0 + Duration::from_millis(100)));
    }

    #[test]
    fn redraw_rate_is_bounded_by_speed() {
        assert_eq!(frame_interval(DEFAULT_SPEED), Duration::from_millis(83));
        assert_eq!(frame_interval(1), Duration::from_millis(1000));
        assert_eq!(frame_interval(60), MIN_FRAME);
    }

    #[test]
    fn a_stalled_loop_does_not_teleport_the_cycles() {
        let t0 = Instant::now();
        let mut wm = tron_wm();
        wm.apply_lightcycle_op(LightCycleOp::Speed(10));
        wm.tick_lightcycle(t0);
        wm.tick_lightcycle(t0 + Duration::from_secs(3600));
        assert_eq!(wm.lightcycle_phase(), Some(2), "capped at 250 ms of travel");
    }
}
