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

use crate::fst::Fst;
use crate::saif::Saif;
use crate::vcd::{Vcd, WindowActivity};

/// A measured per-net toggle-rate source (VCD, SAIF, FST, or one window of a sweep).
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

impl ToggleSource for Fst {
    fn toggle_rate(&self, net: &str) -> f64 {
        Fst::toggle_rate(self, net)
    }
}

/// One window of a sweep is an ordinary toggle source — so a swept run and a single-window
/// run go through the *same* power model, and a per-window number means exactly what the
/// whole-dump number means.
impl ToggleSource for WindowActivity<'_> {
    fn toggle_rate(&self, net: &str) -> f64 {
        WindowActivity::toggle_rate(self, net)
    }
}

/// The activity behind a power number.
///
/// Borrowed sources are allowed (`'a`): a sweep holds one parse of the dump and hands each
/// window out as a borrow, so measuring 400 windows does not clone the dump's name index 400
/// times. An owned source (a whole-dump `Vcd`/`Saif`/`Fst`) satisfies any `'a`.
pub enum Activity<'a> {
    Vectored {
        src: Box<dyn ToggleSource + 'a>,
        fallback_rate: f64,
        label: &'static str,
    },
    Vectorless {
        rate: f64,
    },
}

impl<'a> Activity<'a> {
    pub fn vectorless(activity_factor: f64, freq_hz: f64) -> Self {
        Activity::Vectorless {
            rate: activity_factor * freq_hz,
        }
    }

    /// Vectored activity from any toggle source; `label` names the source in reports.
    pub fn vectored<S: ToggleSource + 'a>(
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
            Activity::Vectored {
                src, fallback_rate, ..
            } => {
                let r = src.toggle_rate(net);
                if r > 0.0 {
                    r
                } else {
                    *fallback_rate
                }
            }
        }
    }

    /// How many of `nets` the activity source actually names.
    ///
    /// (`'a` is the source's lifetime; `'b` here is the caller's net names — unrelated.)
    ///
    /// In `Vectored` mode a net the dump does not mention silently takes the vectorless
    /// fallback — see [`Activity::rate`]. That is the right behaviour and the wrong silence: a
    /// dump covering a fraction of the design still produces a report labelled "vectored", and
    /// the numbers are then mostly the uniform estimate wearing a simulation's name. Callers use
    /// this to say so.
    pub fn coverage<'b>(&self, nets: impl Iterator<Item = &'b str>) -> (usize, usize) {
        match self {
            Activity::Vectorless { .. } => (0, nets.count()),
            Activity::Vectored { src, .. } => {
                let (mut hit, mut total) = (0usize, 0usize);
                for n in nets {
                    total += 1;
                    if src.toggle_rate(n) > 0.0 {
                        hit += 1;
                    }
                }
                (hit, total)
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

#[cfg(test)]
mod coverage_tests {
    use super::*;

    struct Sparse;
    impl ToggleSource for Sparse {
        fn toggle_rate(&self, net: &str) -> f64 {
            if net == "seen" {
                1.0e6
            } else {
                0.0
            }
        }
    }

    #[test]
    fn vectored_coverage_counts_only_the_nets_the_dump_names() {
        // The number that makes a partly-covered dump legible. Without it a report labelled
        // "vectored" over a design the dump barely touches is indistinguishable from one over a
        // design it covers completely — and the difference is whether the figures mean anything.
        let a = Activity::vectored(Sparse, "vectored (test)", 0.2, 1.0e9);
        let (hit, total) = a.coverage(["seen", "unseen", "also_unseen"].into_iter());
        assert_eq!((hit, total), (1, 3));
    }

    #[test]
    fn vectorless_reports_no_net_as_covered() {
        // Honest by construction: in vectorless mode every net takes the uniform estimate, so
        // claiming coverage would be claiming a simulation that was never run.
        let a = Activity::vectorless(0.2, 1.0e9);
        let (hit, total) = a.coverage(["a", "b"].into_iter());
        assert_eq!((hit, total), (0, 2));
    }

    #[test]
    fn a_net_the_dump_does_not_name_still_gets_the_fallback_rate() {
        // The behaviour the coverage number exists to disclose, pinned so the two stay in step:
        // an unnamed net is not zero-power, it silently takes the vectorless estimate.
        let a = Activity::vectored(Sparse, "vectored (test)", 0.2, 1.0e9);
        assert!(
            a.rate("unseen") > 0.0,
            "an unnamed net falls back rather than reading zero"
        );
        assert_eq!(a.rate("seen"), 1.0e6);
    }
}
