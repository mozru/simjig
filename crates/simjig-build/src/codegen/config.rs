//! What the testbench author adds to the IR: probes on internal signals and the interface
//! bindings of an interface-ported top. The `Dut` builder fills a [`CodegenConfig`] in.

/// Inputs of [`lower`](super::lower) besides the IR.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CodegenConfig {
    /// Internal signals made visible to the testbench, in slot order after the ports.
    pub probes: Vec<Probe>,
    /// Set when the DUT has interface ports and the model top is the generated wrapper.
    pub iface_top: Option<IfaceTop>,
}

/// An internal signal the testbench reads or writes through `public_flat_rd`/`_rw`.
///
/// The IR describes ports and types, not internal variables or the instance tree, so a
/// probe names the module that declares the signal (for the `.vlt` line, which matches
/// modules by name) and the signal's type.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Probe {
    /// Hierarchical path from the model top, e.g. `counter.u_core.state`. Its first segment
    /// is the top module; the last is the variable.
    pub path: String,
    /// The module (as declared, not a specialisation name) that declares the variable.
    pub module: String,
    /// The variable's type.
    pub ty: ProbeType,
    /// Read only, or read and write.
    pub access: ProbeAccess,
}

/// The type of a probed variable.
///
/// `#[non_exhaustive]`, like the other public enums that may grow: a `match` outside
/// `simjig-build` needs a wildcard arm.
///
/// ```compile_fail,E0004
/// fn width(ty: &simjig_build::ProbeType) -> Option<u32> {
///     match ty {
///         simjig_build::ProbeType::Named(_) => None,
///         simjig_build::ProbeType::Bits { width } => Some(*width),
///     }
/// }
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ProbeType {
    /// A named IR type, qualified as in the IR (`pkg::state_t`).
    Named(String),
    /// An unsigned vector of `width` bits.
    Bits {
        /// Width in bits, at least 1.
        width: u32,
    },
}

/// How a probe may be accessed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProbeAccess {
    /// `public_flat_rd`: the testbench reads.
    Read,
    /// `public_flat_rw`: the testbench reads and writes (deposits).
    ReadWrite,
}

/// A DUT with interface ports, wrapped in a generated top (DECISION.md table 2, row 9).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IfaceTop {
    /// The DUT module, as declared.
    pub dut: String,
    /// DUT parameter overrides as SystemVerilog expressions. The wrapper is the Verilator
    /// top, so `-G` would reach the wrapper, not the DUT; overrides go here instead.
    pub params: Vec<(String, String)>,
    /// One binding per interface port of the DUT.
    pub bindings: Vec<IfaceBinding>,
}

/// How the wrapper instantiates the interface for one DUT port (`.bind_iface(..)`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IfaceBinding {
    /// The DUT's interface port.
    pub port: String,
    /// The interface to instantiate, as declared.
    pub interface: String,
    /// Interface parameter overrides as SystemVerilog expressions.
    pub params: Vec<(String, String)>,
    /// The modport whose signals the testbench sees. Required for a generic interface port
    /// (no modport in its declaration); otherwise it must equal the declared one or be
    /// `None`.
    pub modport: Option<String>,
}
