// A register that reset never clears (`q`) beside one it does (`r`), and one `$urandom`
// draw at time 0 (`u`), for the initial-value tests (tests/model_tests/init.rs). Built twice:
// `no_reset` (Verilator's default `--x-initial unique`) and `no_reset_fast`
// (`--x-initial fast`, whose initial values are compiled in).
module no_reset (
    input  logic        clk,
    input  logic        rst_n,
    input  logic [31:0] d,
    output logic [31:0] q,
    output logic [31:0] r,
    output logic [31:0] u
);
  initial u = $urandom;
  // No reset, on purpose: the bug random initial values are meant to expose.
  always_ff @(posedge clk) q <= d;
  always_ff @(posedge clk) begin
    if (!rst_n) r <= '0;
    else r <= d;
  end
endmodule
