//! The prediction state of the numeric fields (FORMAT.md section 5).

use crate::math::div_round;

/// One player's or entity's last 16-bit values on one axis.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct AxisTrack {
    last: u16,
    previous: u16,
    before_previous: u16,
    residual: i16,
}

impl AxisTrack {
    #[inline(always)]
    pub(crate) fn velocity(self) -> i32 {
        i32::from(self.last.wrapping_sub(self.previous).cast_signed())
    }
    #[inline(always)]
    pub(crate) fn previous_velocity(self) -> i32 {
        i32::from(
            self.previous
                .wrapping_sub(self.before_previous)
                .cast_signed(),
        )
    }
    #[inline(always)]
    pub(crate) fn last(self) -> u16 {
        self.last
    }
    #[inline(always)]
    pub(crate) fn residual(self) -> i32 {
        i32::from(self.residual)
    }
    /// `residual`: of a 16-bit field, so it fits 16 bits.
    #[inline(always)]
    pub(crate) fn push(&mut self, value: u16, residual: i32) {
        self.before_previous = self.previous;
        self.previous = self.last;
        self.last = value;
        self.residual = residual as i16;
    }
    /// A value left out of its update did not change.
    #[inline(always)]
    pub(crate) fn hold(&mut self) {
        self.push(self.last, 0);
    }
    #[inline(always)]
    pub(crate) fn predict_linear(self) -> u16 {
        self.last
            .wrapping_add(self.velocity().cast_unsigned() as u16)
    }
    /// last + velocity * `time_delta_now` / `time_delta_previous`, rounded, wrapping at 16 bits.
    #[inline(always)]
    pub(crate) fn predict_time_scaled(self, time_deltas: TimeDeltas) -> u16 {
        /// Frame intervals are clamped so the scaled velocity fits 32 bits.
        const TIME_DELTA_MAX: i32 = 32767;
        let TimeDeltas { now, previous } = time_deltas;
        if now == previous || previous <= 0 || now < 0 {
            return self.predict_linear();
        }
        let scaled_velocity = div_round(
            self.velocity() * now.min(TIME_DELTA_MAX),
            previous.min(TIME_DELTA_MAX),
        );
        self.last
            .wrapping_add(scaled_velocity.cast_unsigned() as u16)
    }
}

/// One player's or entity's last float coordinates on one axis, as f32 bits.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct FloatTrack {
    last_bits: u32,
    previous_bits: u32,
    residual: i32,
}

impl FloatTrack {
    #[inline(always)]
    pub(crate) fn residual(&self) -> i32 {
        self.residual
    }
    #[inline(always)]
    pub(crate) fn predict_linear(&self) -> u32 {
        self.last_bits
            .wrapping_add(self.last_bits.wrapping_sub(self.previous_bits))
    }
    #[inline(always)]
    pub(crate) fn push(&mut self, bits: u32, residual: i32) {
        self.previous_bits = self.last_bits;
        self.last_bits = bits;
        self.residual = residual;
    }
    #[inline(always)]
    pub(crate) fn hold(&mut self) {
        self.push(self.last_bits, 0);
    }
}

/// Between a player's frames: this one and the one before.
#[derive(Debug, Clone, Copy)]
pub(crate) struct TimeDeltas {
    now: i32,
    previous: i32,
}

#[inline(always)]
pub(crate) fn time_deltas(frame_times: &mut (u32, u32), time: u32) -> TimeDeltas {
    let (last, previous) = *frame_times;
    *frame_times = (time, last);
    TimeDeltas {
        now: time.wrapping_sub(last).cast_signed(),
        previous: last.wrapping_sub(previous).cast_signed(),
    }
}
