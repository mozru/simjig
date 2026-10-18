// Codegen test design for a sweep over a packed struct whose field widths follow
// parameters: `beat_t` (`data` is `W` bits, `tag` is `T`) is carried by the interface
// `beat_if` on both of `beat_pipe`'s interface ports (`up`, a sink, and `down`, a source)
// and by its own direct ports `peek_in`/`peek_out`.
interface beat_if #(
    parameter int W = 8,
    parameter int T = 2
);
  typedef struct packed {
    logic [W-1:0] data;
    logic [T-1:0] tag;
  } beat_t;
  beat_t beat;
  logic  valid;
  logic  ready;
  modport sink(input beat, input valid, output ready);
  modport source(output beat, output valid, input ready);
endinterface

module beat_pipe #(
    parameter int W = 8,
    parameter int T = 2
) (
    input  logic clk,
    beat_if.sink up,
    beat_if.source down,
    input  logic [W+T-1:0] peek_in,
    output logic [W+T-1:0] peek_out
);
  always_ff @(posedge clk) begin
    down.beat  <= up.beat;
    down.valid <= up.valid;
  end
  assign up.ready = down.ready;
  assign peek_out = peek_in;
endmodule
