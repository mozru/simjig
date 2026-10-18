//! Hand-written instances of the code simjig-build generates for a model, one module per
//! DUT. The `dut!` macro only saves repetition: its expansion is the generated shape
//! (`docs/notes/model.md`, "Generated code"). Slot order, names, widths and access match
//! `shim.cpp`, and the IR hashes match its `kIrHash`.

/// Expands to one generated model module.
macro_rules! dut {
    (
        $module:ident: prefix $prefix:literal, symbol $symbol:literal, ir_hash $hash:literal,
        precision $exp:literal {
            $($slot:literal $field:ident: $handle:ident<$ty:ty> = $name:literal;)*
        }
    ) => {
        pub(crate) mod $module {
            // The value types the field list names; a generator writes full paths instead.
            use simjig_bits::*;

            ::simjig::__private::link_model!(fn vtable = $symbol);

            pub(crate) static MODEL: ::simjig::__private::ModelMeta =
                ::simjig::__private::ModelMeta {
                    vtable,
                    prefix: $prefix,
                    ir_hash: $hash,
                    precision_exponent: $exp,
                    signals: Dut::SIGNALS,
                };

            /// The model's signals.
            #[derive(Clone, Copy, Debug)]
            pub(crate) struct Dut {
                $(pub(crate) $field: ::simjig::__private::$handle<$ty>,)*
            }

            impl Dut {
                pub(crate) const SIGNALS: &'static [::simjig::__private::SignalMeta] = &[$(
                    ::simjig::__private::SignalMeta::new(
                        $name,
                        <$ty as ::simjig::__private::bits::Packed>::WIDTH,
                        ::simjig::__private::SignalAccess::$handle,
                    ),
                )*];

                /// A new simulation of the model and its handles.
                pub(crate) fn sim()
                -> ::core::result::Result<(::simjig::Sim, Self), ::simjig::LoadError> {
                    let (sim, b) = ::simjig::__private::load(&MODEL)?;
                    Ok((sim, Self { $($field: b.bind($slot),)* }))
                }
            }
        }
    };
}

dut!(inv: prefix "Vdut_inv", symbol "simjig_vtable_Vdut_inv", ir_hash 0x5eed_0000_0000_0001,
    precision -12 {
    0 a: In<Bit> = "inv.a";
    1 y: Out<Bit> = "inv.y";
});

dut!(flop: prefix "Vdut_flop", symbol "simjig_vtable_Vdut_flop", ir_hash 0x5eed_0000_0000_0002,
    precision -12 {
    0 clk: In<Bit> = "flop.clk";
    1 rst: In<Bit> = "flop.rst";
    2 d: In<Bit> = "flop.d";
    3 q: Out<Bit> = "flop.q";
});

dut!(counter: prefix "Vdut_counter", symbol "simjig_vtable_Vdut_counter",
    ir_hash 0x5eed_0000_0000_0003, precision -12 {
    0 clk: In<Bit> = "counter.clk";
    1 rst: In<Bit> = "counter.rst";
    2 en: In<Bit> = "counter.en";
    3 count: Out<Bits<8>> = "counter.count";
});

dut!(comb_chain: prefix "Vdut_comb_chain", symbol "simjig_vtable_Vdut_comb_chain",
    ir_hash 0x5eed_0000_0000_0004, precision -12 {
    0 a: In<Bits<8>> = "comb_chain.a";
    1 b: In<Bits<8>> = "comb_chain.b";
    2 y: Out<Bits<8>> = "comb_chain.y";
    3 y_eq: Out<Bit> = "comb_chain.y_eq";
    4 s1: PublicRd<Bits<8>> = "comb_chain.s1";
    5 s2: PublicRd<Bits<8>> = "comb_chain.s2";
    6 s3: PublicRd<Bits<8>> = "comb_chain.s3";
});

dut!(fatal: prefix "Vdut_fatal", symbol "simjig_vtable_Vdut_fatal", ir_hash 0x5eed_0000_0000_0005,
    precision -12 {
    0 clk: In<Bit> = "fatal.clk";
    1 rst: In<Bit> = "fatal.rst";
    2 fire: In<Bit> = "fatal.fire";
    3 kind: In<Bits<2>> = "fatal.kind";
    4 alive: Out<Bits<8>> = "fatal.alive";
});

dut!(wide: prefix "Vdut_wide", symbol "simjig_vtable_Vdut_wide", ir_hash 0x5eed_0000_0000_0006,
    precision -12 {
    0 clk: In<Bit> = "wide.clk";
    1 i1: In<Bit> = "wide.i1";
    2 i8: In<Bits<8>> = "wide.i8";
    3 i33: In<Bits<33>> = "wide.i33";
    4 i64: In<Bits<64>> = "wide.i64";
    5 i65: In<Bits<65>> = "wide.i65";
    6 i200: In<WideBits<200, 7>> = "wide.i200";
    7 o1: Out<Bit> = "wide.o1";
    8 o8: Out<Bits<8>> = "wide.o8";
    9 o33: Out<Bits<33>> = "wide.o33";
    10 o64: Out<Bits<64>> = "wide.o64";
    11 o65: Out<Bits<65>> = "wide.o65";
    12 o200: Out<WideBits<200, 7>> = "wide.o200";
    13 q200: Out<WideBits<200, 7>> = "wide.q200";
});

dut!(timing: prefix "Vdut_timing", symbol "simjig_vtable_Vdut_timing",
    ir_hash 0x5eed_0000_0000_0007, precision -12 {
    0 start: In<Bit> = "timing_dut.start";
    1 iclk_out: Out<Bit> = "timing_dut.iclk_out";
    2 ticks: Out<Bits<8>> = "timing_dut.ticks";
    3 done: Out<Bit> = "timing_dut.done";
});

dut!(hello: prefix "Vhello", symbol "simjig_vtable_Vhello", ir_hash 0x5eed_0000_0000_0008,
    precision -12 {
    0 clk: In<Bit> = "hello.clk";
    1 n: Out<Bits<8>> = "hello.n";
});
