// Codegen test design for parameter sweeps: `sw_top` is built at two parameter sets.
// - `clk`, `fixed`, `hdr_in`/`hdr_out` (a package struct) are alike in every set;
// - `a`, `b` and `sum` follow `W` (`a` and `b` always agree);
// - `word` is a struct declared in the module, whose field follows `W`;
// - `lanes` is an unpacked array of `N` entries.
// Non-ANSI ports, so the port list can name a type the module declares.
package sw_pkg;
  typedef enum logic [1:0] {
    ST_IDLE,
    ST_BUSY
  } state_e;
  typedef struct packed {
    state_e     state;
    logic [3:0] tag;
  } hdr_t;
  localparam int TAGS = 16;
endpackage

module sw_top (clk, a, b, sum, hdr_in, hdr_out, word, lanes, fixed);
  parameter int W = 8;
  parameter int N = 2;
  typedef struct packed {
    logic [W-1:0] data;
    logic         last;
  } word_t;
  input logic clk;
  input logic [W-1:0] a;
  input logic [W-1:0] b;
  output logic [W:0] sum;
  input sw_pkg::hdr_t hdr_in;
  output sw_pkg::hdr_t hdr_out;
  output word_t word;
  output logic [7:0] lanes[N];
  output logic [3:0] fixed;
  timeunit 1ns; timeprecision 1ps;

  always_ff @(posedge clk) begin
    sum <= a + b;
    hdr_out <= hdr_in;
    word <= '{data: a, last: 1'b1};
  end
  for (genvar i = 0; i < N; i++) begin : g_lane
    assign lanes[i] = 8'(i);
  end
  assign fixed = 4'h5;
endmodule
