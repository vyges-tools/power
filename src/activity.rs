//! Activity model: per-net **toggle rate** (transitions/sec).
//!
//! Two sources, the two ways power flows are obtained in practice:
//! - **vectored** — measured per-net rates from a VCD (nets absent from the dump
//!   fall back to the vectorless estimate, so they are never silently zeroed);
//! - **vectorless** — a uniform `activity_factor × clock_frequency` when no
//!   simulation exists. (Depth reserved: probabilistic signal-probability /
//!   toggle-rate propagation through the netlist.)

use crate::vcd::Vcd;

pub enum Activity {
    Vectored { vcd: Vcd, fallback_rate: f64 },
    Vectorless { rate: f64 },
}

impl Activity {
    pub fn vectorless(activity_factor: f64, freq_hz: f64) -> Self {
        Activity::Vectorless { rate: activity_factor * freq_hz }
    }

    pub fn vectored(vcd: Vcd, activity_factor: f64, freq_hz: f64) -> Self {
        Activity::Vectored { vcd, fallback_rate: activity_factor * freq_hz }
    }

    /// Transitions/sec for a net.
    pub fn rate(&self, net: &str) -> f64 {
        match self {
            Activity::Vectorless { rate } => *rate,
            Activity::Vectored { vcd, fallback_rate } => {
                let r = vcd.toggle_rate(net);
                if r > 0.0 {
                    r
                } else {
                    *fallback_rate
                }
            }
        }
    }

    pub fn mode(&self) -> &'static str {
        match self {
            Activity::Vectored { .. } => "vectored (VCD)",
            Activity::Vectorless { .. } => "vectorless",
        }
    }
}
