//! Loading a generated model and binding its typed handles: the [`Binder`] trait that
//! generated `Dut::__bind` calls, its implementation [`ModelBinder`], and
//! [`dut_model!`](crate::__dut_model), which makes a generated `Dut` a
//! [`DutModel`](crate::DutModel).
//!
//! For each model, simjig-build's codegen emits the `Dut` struct with `IR_HASH`, `SIGNALS`
//! and `__bind`; next to it the generated file holds one line naming the model's C side:
//!
//! ```text
//! ::simjig::__private::dut_model!(
//!     Dut,
//!     prefix = "Vcounter",
//!     symbol = "simjig_vtable_Vcounter",
//!     precision_exponent = -12,
//! );
//! ```
//!
//! which expands to the vtable link ([`link_model!`](crate::__link_model)), a `static`
//! [`ModelMeta`] and `impl DutModel for Dut` calling [`instantiate`] and [`create`]. The
//! harness calls `instantiate` (model construction and every load check) and then `create`
//! (handle binding) once per test; a test with several DUTs calls [`load`] and
//! [`create_in`] for each instead.

use super::meta::{Access, LoadError, ModelMeta, SignalMeta};
use super::verilator::VerilatorModel;
use crate::harness::dut::{LoadedModel, ModelOptions};
use crate::kernel::failure::{Failure, FailureKind};
use crate::kernel::signal::RawSig;
use crate::kernel::sim::Sim;
use crate::kernel::time::{Precision, SimTime};
use crate::signal::{FromRaw, In, InOut, Out, PublicRd, PublicRw};

/// Makes the typed handles of one model while generated `Dut::__bind` runs; generated code
/// calls the method matching each signal's [`Access`], in slot order.
pub trait Binder {
    /// The handle of the input `meta`.
    fn input<T: 'static>(&mut self, meta: &'static SignalMeta) -> In<T>;
    /// The handle of the output `meta`.
    fn output<T: 'static>(&mut self, meta: &'static SignalMeta) -> Out<T>;
    /// The handle of the inout `meta`.
    fn inout<T: 'static>(&mut self, meta: &'static SignalMeta) -> InOut<T>;
    /// The handle of the `public_flat_rd` signal `meta`.
    fn public_rd<T: 'static>(&mut self, meta: &'static SignalMeta) -> PublicRd<T>;
    /// The handle of the `public_flat_rw` signal `meta`.
    fn public_rw<T: 'static>(&mut self, meta: &'static SignalMeta) -> PublicRw<T>;
}

/// The [`Binder`] of a loaded model; made by [`create`].
#[derive(Debug)]
pub struct ModelBinder {
    sim_gen: u32,
    /// The model's index in its simulation.
    model: u32,
    signals: &'static [SignalMeta],
}

impl ModelBinder {
    /// The raw handle of `meta`, after checking that it is this model's entry for its slot
    /// and has the access the caller binds it as.
    #[track_caller]
    fn raw(&self, meta: &'static SignalMeta, access: Access) -> RawSig {
        let ours = self.signals.get(meta.slot as usize);
        assert!(
            ours == Some(meta),
            "generated code binds `{}` (slot {}), which is not this model's signal of that slot",
            meta.name,
            meta.slot
        );
        assert!(
            meta.access == access,
            "generated code binds `{}` ({:?}) as {access:?}",
            meta.name,
            meta.access
        );
        RawSig::in_model(self.sim_gen, self.model, meta.slot)
    }
}

impl Binder for ModelBinder {
    #[track_caller]
    fn input<T: 'static>(&mut self, meta: &'static SignalMeta) -> In<T> {
        In::from_raw(self.raw(meta, Access::In))
    }

    #[track_caller]
    fn output<T: 'static>(&mut self, meta: &'static SignalMeta) -> Out<T> {
        Out::from_raw(self.raw(meta, Access::Out))
    }

    #[track_caller]
    fn inout<T: 'static>(&mut self, meta: &'static SignalMeta) -> InOut<T> {
        InOut::from_raw(self.raw(meta, Access::InOut))
    }

    #[track_caller]
    fn public_rd<T: 'static>(&mut self, meta: &'static SignalMeta) -> PublicRd<T> {
        PublicRd::from_raw(self.raw(meta, Access::PublicRd))
    }

    #[track_caller]
    fn public_rw<T: 'static>(&mut self, meta: &'static SignalMeta) -> PublicRw<T> {
        PublicRw::from_raw(self.raw(meta, Access::PublicRw))
    }
}

/// Loads the model `meta` describes into a new [`Sim`] (the `DutModel::instantiate` of a
/// generated `Dut`).
///
/// # Errors
/// A `ModelFatal` [`Failure`] at time 0 carrying the [`LoadError`]'s text: the model and the
/// generated Rust disagree, or the model failed while being constructed.
#[expect(
    clippy::result_large_err,
    reason = "the DutModel signature; called once per test"
)]
pub fn instantiate(meta: &'static ModelMeta, options: &ModelOptions) -> Result<Sim, Failure> {
    let model = load(meta, options)?;
    Ok(Sim::from_backend(model.into_backend()))
}

/// Loads the model `meta` describes on its own, to share a [`Sim`] with other models (the
/// `DutModel::load` of a generated `Dut`).
///
/// # Errors
/// As [`instantiate`].
#[expect(
    clippy::result_large_err,
    reason = "the DutModel signature; called once per test"
)]
pub fn load(meta: &'static ModelMeta, options: &ModelOptions) -> Result<LoadedModel, Failure> {
    let model = VerilatorModel::load(
        meta,
        &options.verilator_args(),
        options.random_init(),
        options.trace.as_ref(),
    )
    .map_err(|e| {
        let precision = Precision::from_exponent(meta.precision_exponent).unwrap_or(Precision::PS);
        load_failure(meta, &e, precision)
    })?;
    Ok(LoadedModel::from_backend(Box::new(model)))
}

/// Binds a generated `Dut` to `sim` with its `__bind` (the `DutModel::create` of a generated
/// `Dut`).
///
/// # Errors
/// A `ModelFatal` [`Failure`] if `sim` does not run the model `meta` describes.
#[expect(
    clippy::result_large_err,
    reason = "the DutModel signature; called once per test"
)]
pub fn create<D>(
    sim: &Sim,
    meta: &'static ModelMeta,
    bind: fn(&mut ModelBinder) -> D,
) -> Result<D, Failure> {
    create_in(sim, 0, meta, bind)
}

/// Binds a generated `Dut` to model number `model` of `sim` (the `DutModel::bind` of a
/// generated `Dut`).
///
/// # Errors
/// A `ModelFatal` [`Failure`] if model `model` of `sim` is not the model `meta` describes.
#[expect(
    clippy::result_large_err,
    reason = "the DutModel signature; called once per test"
)]
pub fn create_in<D>(
    sim: &Sim,
    model: usize,
    meta: &'static ModelMeta,
    bind: fn(&mut ModelBinder) -> D,
) -> Result<D, Failure> {
    let mut sim_gen = None;
    for s in meta.signals {
        match sim.signal_in(model, s.name) {
            Some(raw) if raw.local_slot() == s.slot => sim_gen = Some(raw.sim_gen()),
            _ => {
                let error = LoadError::SignalTable {
                    prefix: meta.prefix,
                    slot: s.slot as usize,
                    problem: format!("the simulation has no `{}` in this slot", s.name),
                };
                return Err(load_failure(meta, &error, sim.precision()));
            }
        }
    }
    let mut binder = ModelBinder {
        // A model without signals binds nothing, so its id is never used.
        sim_gen: sim_gen.unwrap_or(0),
        // A model index past u32 has no signals either (the lookup above found none).
        model: u32::try_from(model).unwrap_or(u32::MAX),
        signals: meta.signals,
    };
    Ok(bind(&mut binder))
}

/// The failure of loading the model of DUT `meta`: its message names the DUT, then the
/// problem; a setup problem's advice goes in its help, not its message.
fn load_failure(meta: &ModelMeta, error: &LoadError, precision: Precision) -> Failure {
    let message = format!("DUT `{}`: {error}", meta.dut_name());
    let kind = match error.help() {
        Some(help) => FailureKind::Setup {
            message: message.clone(),
            help,
        },
        None => FailureKind::ModelFatal {
            message: message.clone(),
        },
    };
    Failure {
        kind,
        time: SimTime::ZERO,
        precision,
        task: None,
        location: None,
        message,
    }
}

/// Implements [`DutModel`](crate::DutModel) for a generated `Dut` over its Verilator model;
/// see the `model::bind` module docs. For generated code only.
#[doc(hidden)]
#[macro_export]
macro_rules! __dut_model {
    (
        $dut:ident,
        prefix = $prefix:literal,
        symbol = $symbol:literal,
        precision_exponent = $exp:literal
        $(, dpi_imports = [$($dpi:literal),* $(,)?])? $(,)?
    ) => {
        const _: () = {
    $crate::__private::link_model!(fn vtable = $symbol);

            static MODEL: $crate::__private::ModelMeta = $crate::__private::ModelMeta {
                vtable,
                prefix: $prefix,
                ir_hash: $dut::IR_HASH,
                precision_exponent: $exp,
                signals: $dut::SIGNALS,
                module: ::core::module_path!(),
                dpi_imports: &[$($($dpi),*)?],
            };

            impl $crate::DutModel for $dut {
                fn instantiate(
                    options: &$crate::ModelOptions,
                ) -> ::core::result::Result<$crate::Sim, $crate::Failure> {
                    $crate::__private::instantiate(&MODEL, options)
                }

                fn create(sim: &mut $crate::Sim) -> ::core::result::Result<Self, $crate::Failure> {
                    $crate::__private::create(sim, &MODEL, $dut::__bind)
                }

                fn load(
                    options: &$crate::ModelOptions,
                ) -> ::core::result::Result<$crate::LoadedModel, $crate::Failure> {
                    $crate::__private::load(&MODEL, options)
                }

                fn bind(
                    sim: &mut $crate::Sim,
                    model: usize,
                ) -> ::core::result::Result<Self, $crate::Failure> {
                    $crate::__private::create_in(sim, model, &MODEL, $dut::__bind)
                }

                fn design() -> ::core::option::Option<&'static str> {
                    MODEL.top()
                }
            }
        };
    };
}
