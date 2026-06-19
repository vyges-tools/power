//! Activity model: per-net **toggle rate** (transitions/sec).
//!
//! Two sources, the two ways power flows are obtained in practice:
//! - **vectored** — measured per-net rates from a simulation, either a **VCD**
//!   ([`crate::vcd::Vcd`]) or a **SAIF** ([`crate::saif::Saif`], e.g. Verilator
//!   `--trace-saif`). Both expose a per-net toggle rate behind the [`ToggleSource`]
//!   trait; nets absent from the dump fall back to the vectorless estimate, so
//!   they are never silently zeroed.
//! - **vectorless** — a uniform `activity_factor × clock_frequency` when no
//!   simulation exists. (Depth reserved: probabilistic signal-probability /
//!   toggle-rate propagation through the netlist.)

use crate::saif::Saif;
use crate::vcd::Vcd;

/// A measured per-net toggle-rate source (VCD or SAIF).
pub trait ToggleSource {
    fn toggle_rate(&self, net: &str) -> f64;
}

impl ToggleSource for Vcd {
    fn toggle_rate(&self, net: &str) -> f64 {
        Vcd::toggle_rate(self, net)
    }
}

impl ToggleSource for Saif {
    fn toggle_rate(&self, net: &str) -> f64 {
        Saif::toggle_rate(self, net)
    }
}

pub enum Activity {
    Vectored { src: Box<dyn ToggleSource>, fallback_rate: f64, label: &'static str },
    Vectorless { rate: f64 },
}

impl Activity {
    pub fn vectorless(activity_factor: f64, freq_hz: f64) -> Self {
        Activity::Vectorless { rate: activity_factor * freq_hz }
    }

    /// Vectored activity from any toggle source; `label` names the source in reports.
    pub fn vectored<S: ToggleSource + 'static>(
        src: S,
        label: &'static str,
        activity_factor: f64,
        freq_hz: f64,
    ) -> Self {
        Activity::Vectored {
            src: Box::new(src),
            fallback_rate: activity_factor * freq_hz,
            label,
        }
    }

    /// Transitions/sec for a net.
    pub fn rate(&self, net: &str) -> f64 {
        match self {
            Activity::Vectorless { rate } => *rate,
            Activity::Vectored { src, fallback_rate, .. } => {
                let r = src.toggle_rate(net);
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
            Activity::Vectored { label, .. } => label,
            Activity::Vectorless { .. } => "vectorless",
        }
    }
}
