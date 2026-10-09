//! Motion. Little moves here, and what does is something the hand reached
//! for rarely and on purpose: a picture brought forward, a menu of settings
//! opened. What the keyboard asks for many times a day (the command menu,
//! the selection within it) does not move at all.
//!
//! A `Tween` is a number on its way somewhere. Sent somewhere new while
//! still moving, it leaves from where it is, so nothing ever jumps.

use std::time::{Duration, Instant};

/// Arriving: for something the user asked to see.
pub const ENTER: Duration = Duration::from_millis(200);
/// Leaving is quicker: nobody waits to watch a thing go.
pub const EXIT: Duration = Duration::from_millis(130);

/// Fast away and long to settle: `cubic-bezier(0.23, 1, 0.32, 1)`, near enough.
pub fn ease_out(t: f32) -> f32 {
    1.0 - (1.0 - t.clamp(0.0, 1.0)).powi(5)
}

#[derive(Clone, Copy, Debug)]
pub struct Tween {
    from: f32,
    to: f32,
    start: Instant,
    span: Duration,
}

impl Tween {
    /// At rest on `value`.
    pub fn at(value: f32) -> Tween {
        Tween {
            from: value,
            to: value,
            start: Instant::now(),
            span: Duration::ZERO,
        }
    }

    /// Heads for `to`, from wherever it is now. Already headed there, it
    /// carries on as it was.
    pub fn go(&mut self, to: f32, span: Duration, now: Instant) {
        if (to - self.to).abs() > f32::EPSILON {
            *self = Tween {
                from: self.value(now),
                to,
                start: now,
                span,
            };
        }
    }

    pub fn value(&self, now: Instant) -> f32 {
        if self.span.is_zero() {
            return self.to;
        }
        let t = now.saturating_duration_since(self.start).as_secs_f32() / self.span.as_secs_f32();
        self.from + (self.to - self.from) * ease_out(t)
    }

    pub fn moving(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.start) < self.span
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tween_turned_round_leaves_from_where_it_is() {
        let start = Instant::now();
        let later = |ms: u64| start + Duration::from_millis(ms);
        let mut tween = Tween::at(0.0);
        assert!(!tween.moving(start));
        tween.go(1.0, ENTER, start);
        assert_eq!(tween.value(start), 0.0);
        let midway = tween.value(later(60));
        assert!(midway > 0.5 && midway < 1.0, "{midway}");
        assert!(tween.moving(later(60)));
        // Sent back before it arrived: no jump to the end first.
        tween.go(0.0, EXIT, later(60));
        assert_eq!(tween.value(later(60)), midway);
        assert!(tween.value(later(90)) < midway);
        assert_eq!(tween.value(later(60 + 130)), 0.0);
        assert!(!tween.moving(later(60 + 130)));
        // Told to go where it is already going, it does not start again.
        tween.go(1.0, ENTER, later(300));
        tween.go(1.0, ENTER, later(400));
        assert_eq!(tween.value(later(500)), 1.0);
        // Easing ends where it should and never runs past it.
        assert_eq!((ease_out(0.0), ease_out(1.0), ease_out(7.0)), (0.0, 1.0, 1.0));
    }
}
