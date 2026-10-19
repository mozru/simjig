// The benchmark's FIFO for Marlin: `sync_fifo` with WIDTH=8, DEPTH=16, FWFT=0 (the
// parameters every other flow passes as -G...), wrapped because Marlin's `#[verilog]`
// macro reads port widths from the top module's source and has no way to pass
// parameters: `count`'s width, `[$clog2(DEPTH):0]`, must appear here as a literal.
// The level is inlined away (a comment must not start with the tool's name: that is
// a pragma); the model is the same as the other flows'.
module sync_fifo_bench (
    input  logic       clk,
    input  logic       rst,
    input  logic       push,
    input  logic [7:0] wr_data,
    input  logic       pop,
    output logic [7:0] rd_data,
    output logic       full,
    output logic       empty,
    output logic [4:0] count
);
  timeunit 1ns; timeprecision 1ps;  // as sync_fifo's own (Verilator's TIMESCALEMOD)

  sync_fifo #(.WIDTH(8), .DEPTH(16), .FWFT(1'b0)) u_fifo (.*);
endmodule
