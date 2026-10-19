//! `struct_pipe`: the generated packed struct `Beat` and enum `Op`, typed comparisons, and
//! the field-by-field diff a mismatch reports.

use simjig::{clock, ns, read_only};
use simjig_bits::{Bit, Bits};

use crate::struct_pipe::{Beat, Dut, Op, OpVariant};

/// What the pipeline computes for `beat` with `operand` (`apply` in `struct_pipe.sv`).
fn model(beat: Beat, operand: u128) -> Beat {
    let x = beat.payload.to_u128();
    let payload = match beat.op.variant() {
        Ok(OpVariant::Add) => x + operand,
        Ok(OpVariant::Xor) => x ^ operand,
        Ok(OpVariant::Inv) => !x,
        Ok(OpVariant::Nop) | Err(_) => x,
    };
    Beat {
        payload: Bits::new(payload & 0xfff),
        ..beat
    }
}

/// Resets the pipeline, then sends `beats` one per cycle with `operand` and returns what
/// came out, two cycles later each.
async fn run(dut: Dut, operand: u128, beats: &[Beat]) -> Vec<Beat> {
    clock(dut.clk, ns(10)).start();
    dut.rst.set(1);
    dut.in_valid.set(0);
    dut.operand.set(operand);
    dut.clk.rising().await;
    dut.clk.falling().await;
    dut.rst.set(0);
    let mut out = Vec::new();
    for i in 0..beats.len() + 2 {
        if let Some(&beat) = beats.get(i) {
            dut.in_beat.set(beat);
            dut.in_valid.set(1);
        } else {
            dut.in_valid.set(0);
        }
        dut.clk.rising().await;
        read_only().await;
        if dut.out_valid.get() == Bit::ONE {
            out.push(dut.out_beat.get());
        }
        dut.clk.falling().await;
    }
    out
}

fn beats() -> Vec<Beat> {
    [
        (Op::ADD, 0x1, 0x100),
        (Op::XOR, 0x2, 0x0f0),
        (Op::INV, 0x3, 0x00f),
        (Op::NOP, 0xf, 0xabc),
    ]
    .map(|(op, tag, payload)| Beat {
        op,
        tag: Bits::new(tag),
        payload: Bits::new(payload),
    })
    .to_vec()
}

#[simjig::test(timeout = "1us")]
async fn beats_come_out_transformed_two_cycles_later(dut: Dut) {
    let sent = beats();
    let got = run(dut, 0x011, &sent).await;
    let want: Vec<Beat> = sent.iter().map(|&b| model(b, 0x011)).collect();
    assert_eq!(got, want);
    assert_eq!(got[2].op, Op::INV);
    assert_eq!(format!("{:?}", got[0].op), "OP_ADD");
}

/// A deliberately wrong expectation (the payload with the wrong operand) fails with the
/// differing field named by its SystemVerilog path.
#[simjig::test(timeout = "1us")]
#[should_panic(expected = "beat 0 differs: payload: 12'h111 != 12'h110")]
async fn a_mismatch_names_the_differing_field(dut: Dut) {
    let sent = beats();
    let got = run(dut, 0x011, &sent).await;
    let wrong = Beat {
        payload: Bits::new(0x100 + 0x010),
        ..sent[0]
    };
    let diffs: Vec<String> = got[0]
        .diff(&wrong)
        .iter()
        .map(ToString::to_string)
        .collect();
    assert!(diffs.is_empty(), "beat 0 differs: {}", diffs.join(", "));
}
