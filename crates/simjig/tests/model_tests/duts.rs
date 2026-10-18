//! Hand-written instances of the code simjig-build generates for a model, one module per
//! DUT. The `dut!` macro only saves repetition: its expansion is the generated shape
//! (`docs/notes/model.md`, "Generated code"). Slot order, names, widths and access match
//! `shim.cpp`, and the IR hashes match its `kIrHash`.

/// Expands to one generated model module: what codegen emits (`Dut` with `IR_HASH`,
/// `SIGNALS` and `__bind`) plus the `dut_model!` line naming the model's C side. The one
/// addition to codegen's output is `#[derive(Clone, Copy, Debug)]` on `Dut` (proposed in
/// `docs/notes/model.md`), so tasks can take copies of the handles.
macro_rules! dut {
    (
        $module:ident: prefix $prefix:literal, symbol $symbol:literal, ir_hash $hash:literal,
        precision $exp:literal {
            $($slot:literal $field:ident: $handle:ident<$ty:ty> = $name:literal, $bind:ident;)*
        }
    ) => {
        pub(crate) mod $module {
            // The value types the field list names; codegen writes full paths instead.
            use simjig_bits::*;

            /// The model's signals.
            #[derive(Clone, Copy, Debug)]
            pub(crate) struct Dut {
                $(pub(crate) $field: ::simjig::__private::$handle<$ty>,)*
            }

            impl Dut {
                pub(crate) const IR_HASH: u64 = $hash;

                pub(crate) const SIGNALS: &'static [::simjig::__private::SignalMeta] = &[$(
                    ::simjig::__private::SignalMeta {
                        name: $name,
                        slot: $slot,
                        width: <$ty as ::simjig::__private::bits::Packed>::WIDTH,
                        storage: storage(<$ty as ::simjig::__private::bits::Packed>::WIDTH),
                        access: ::simjig::__private::Access::$handle,
                    },
                )*];

                #[doc(hidden)]
                pub(crate) fn __bind(binder: &mut impl ::simjig::__private::Binder) -> Self {
                    Self {
                        $($field: ::simjig::__private::Binder::$bind(binder, &Self::SIGNALS[$slot]),)*
                    }
                }
            }

            /// Codegen writes the storage literally; the tests compute it.
            const fn storage(width: u32) -> ::simjig::__private::Storage {
                use ::simjig::__private::Storage;
                match width {
                    0..=8 => Storage::CData,
                    9..=16 => Storage::SData,
                    17..=32 => Storage::IData,
                    33..=64 => Storage::QData,
                    _ => Storage::VlWide { words: width.div_ceil(32) },
                }
            }

            ::simjig::__private::dut_model!(
                Dut,
                prefix = $prefix,
                symbol = $symbol,
                precision_exponent = $exp,
            );
        }
    };
}

/// A new simulation of `D` and its handles, as the harness builds them.
pub(crate) fn sim<D: simjig::DutModel>() -> (simjig::Sim, D) {
    let options = simjig::ModelOptions::new(simjig::Seed::new(1, simjig::SeedSource::Env));
    let mut sim = D::instantiate(&options).unwrap();
    let dut = D::create(&mut sim).unwrap();
    (sim, dut)
}

dut!(inv: prefix "Vdut_inv", symbol "simjig_vtable_Vdut_inv", ir_hash 0x5eed_0000_0000_0001,
    precision -12 {
    0 a: In<Bit> = "inv.a", input;
    1 y: Out<Bit> = "inv.y", output;
});

dut!(flop: prefix "Vdut_flop", symbol "simjig_vtable_Vdut_flop", ir_hash 0x5eed_0000_0000_0002,
    precision -12 {
    0 clk: In<Bit> = "flop.clk", input;
    1 rst: In<Bit> = "flop.rst", input;
    2 d: In<Bit> = "flop.d", input;
    3 q: Out<Bit> = "flop.q", output;
});

dut!(counter: prefix "Vdut_counter", symbol "simjig_vtable_Vdut_counter",
    ir_hash 0x5eed_0000_0000_0003, precision -12 {
    0 clk: In<Bit> = "counter.clk", input;
    1 rst: In<Bit> = "counter.rst", input;
    2 en: In<Bit> = "counter.en", input;
    3 count: Out<Bits<8>> = "counter.count", output;
});

dut!(comb_chain: prefix "Vdut_comb_chain", symbol "simjig_vtable_Vdut_comb_chain",
    ir_hash 0x5eed_0000_0000_0004, precision -12 {
    0 a: In<Bits<8>> = "comb_chain.a", input;
    1 b: In<Bits<8>> = "comb_chain.b", input;
    2 y: Out<Bits<8>> = "comb_chain.y", output;
    3 y_eq: Out<Bit> = "comb_chain.y_eq", output;
    4 s1: PublicRd<Bits<8>> = "comb_chain.s1", public_rd;
    5 s2: PublicRd<Bits<8>> = "comb_chain.s2", public_rd;
    6 s3: PublicRd<Bits<8>> = "comb_chain.s3", public_rd;
});

dut!(fatal: prefix "Vdut_fatal", symbol "simjig_vtable_Vdut_fatal", ir_hash 0x5eed_0000_0000_0005,
    precision -12 {
    0 clk: In<Bit> = "fatal.clk", input;
    1 rst: In<Bit> = "fatal.rst", input;
    2 fire: In<Bit> = "fatal.fire", input;
    3 kind: In<Bits<2>> = "fatal.kind", input;
    4 alive: Out<Bits<8>> = "fatal.alive", output;
});

dut!(wide: prefix "Vdut_wide", symbol "simjig_vtable_Vdut_wide", ir_hash 0x5eed_0000_0000_0006,
    precision -12 {
    0 clk: In<Bit> = "wide.clk", input;
    1 i1: In<Bit> = "wide.i1", input;
    2 i8: In<Bits<8>> = "wide.i8", input;
    3 i33: In<Bits<33>> = "wide.i33", input;
    4 i64: In<Bits<64>> = "wide.i64", input;
    5 i65: In<Bits<65>> = "wide.i65", input;
    6 i200: In<WideBits<200, 7>> = "wide.i200", input;
    7 o1: Out<Bit> = "wide.o1", output;
    8 o8: Out<Bits<8>> = "wide.o8", output;
    9 o33: Out<Bits<33>> = "wide.o33", output;
    10 o64: Out<Bits<64>> = "wide.o64", output;
    11 o65: Out<Bits<65>> = "wide.o65", output;
    12 o200: Out<WideBits<200, 7>> = "wide.o200", output;
    13 q200: Out<WideBits<200, 7>> = "wide.q200", output;
});

dut!(timing: prefix "Vdut_timing", symbol "simjig_vtable_Vdut_timing",
    ir_hash 0x5eed_0000_0000_0007, precision -12 {
    0 start: In<Bit> = "timing_dut.start", input;
    1 iclk_out: Out<Bit> = "timing_dut.iclk_out", output;
    2 ticks: Out<Bits<8>> = "timing_dut.ticks", output;
    3 done: Out<Bit> = "timing_dut.done", output;
});

dut!(hello: prefix "Vhello", symbol "simjig_vtable_Vhello", ir_hash 0x5eed_0000_0000_0008,
    precision -12 {
    0 clk: In<Bit> = "hello.clk", input;
    1 n: Out<Bits<8>> = "hello.n", output;
});
