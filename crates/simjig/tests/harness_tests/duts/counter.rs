// A hand-written stand-in for simjig-build's generated bindings (see `include_dut!`): an
// accumulator on a FakeModel. On each rising edge of `clk` (a 10 ns kernel clock), if `en`
// is set, `acc` += `data`.
//
// Plain `//`, not `//!`: the file is pulled in with `include!`, which rejects inner doc
// comments (E0753); the module's doc comment comes from `include_dut!`.

/// The DUT handles.
#[derive(Debug, Clone, Copy)]
pub struct Dut {
    /// The clock, driven by the kernel.
    pub clk: ::simjig::RawSig,
    /// Accumulate enable.
    pub en: ::simjig::RawSig,
    /// The value to add (8 bits).
    pub data: ::simjig::RawSig,
    /// The sum (16 bits).
    pub acc: ::simjig::RawSig,
}

impl ::simjig::DutModel for Dut {
    fn instantiate(
        _options: &::simjig::ModelOptions,
    ) -> ::core::result::Result<::simjig::Sim, ::simjig::Failure> {
        let mut m = ::simjig::FakeModel::new(::simjig::Precision::NS);
        let clk = m.signal("clk", 1);
        let en = m.signal("en", 1);
        let data = m.signal("data", 8);
        let acc = m.signal("acc", 16);
        m.on_rising(clk, move |s| {
            if s.get(en) == 1 {
                s.set(acc, s.get(acc) + s.get(data));
            }
        });
        Ok(::simjig::Sim::from_fake(m))
    }

    fn create(sim: &mut ::simjig::Sim) -> ::core::result::Result<Self, ::simjig::Failure> {
        let sig = |name: &str| sim.signal(name);
        let (Some(clk), Some(en), Some(data), Some(acc)) =
            (sig("clk"), sig("en"), sig("data"), sig("acc"))
        else {
            return Err(setup_failure(sim, "the model lacks a counter signal".into()));
        };
        let dut = Self { clk, en, data, acc };
        sim.add_clock(::simjig::Clock::new(
            dut.clk,
            ::simjig::SimDuration::from_ticks(10),
        ))
        .map_err(|e| setup_failure(sim, e.to_string()))?;
        Ok(dut)
    }
}

/// A failure while binding the DUT, before the simulation starts.
pub fn setup_failure(sim: &::simjig::Sim, message: String) -> ::simjig::Failure {
    ::simjig::Failure {
        kind: ::simjig::FailureKind::ModelFatal {
            message: message.clone(),
        },
        time: ::simjig::SimTime::ZERO,
        precision: sim.precision(),
        task: None,
        location: None,
        message,
    }
}
