// The benchmark's mid-size design for Marlin: `mid_pipe` with its default parameters
// (STAGES=128, DEPTH=256, which no other flow overrides either), wrapped because Marlin's
// `#[verilog]` macro reads port widths from the top module's source: `count`'s width,
// `[$clog2(DEPTH):0]`, must appear here as a literal. As sync_fifo_bench.sv, the level
// is inlined away; the model is the same as the other flows'.
module mid_pipe_bench (
    input  logic        clk,
    input  logic        rst,
    input  logic        push,
    input  logic [63:0] wr_data,
    input  logic        pop,
    output logic        full,
    output logic        empty,
    output logic [8:0]  count,
    output logic        out_valid,
    output logic [63:0] out_data,
    output logic [31:0] out_crc
);
  timeunit 1ns; timeprecision 1ps;  // as mid_pipe's own (Verilator's TIMESCALEMOD)

  mid_pipe #(.STAGES(128), .DEPTH(256)) u_mid (.*);
endmodule
