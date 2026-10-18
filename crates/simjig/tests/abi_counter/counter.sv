// Test model for tests/abi_counter.rs: one port per Verilator storage class (C, S, I, Q, W),
// plus a `cmd` input that selects a system task on the next rising edge. The `*_in` ports
// narrower than their storage (5 bits in CData, 12 in SData, 40 in QData) and their
// zero-extended `*_ext` copies show whether a write left the unused upper bits clean:
// the model assumes they are zero and does not mask them when it widens a value.
module counter (
    input  logic        clk,
    input  logic        rst,
    input  logic [ 7:0] cmd,
    input  logic [15:0] half_in,
    input  logic [99:0] wide_in,
    input  logic [ 4:0] tiny_in,
    input  logic [11:0] mid_in,
    input  logic [39:0] quad_in,
    output logic [31:0] count,
    output logic [15:0] half_out,
    output logic [39:0] quad_out,
    output logic [99:0] wide_out,
    output logic [ 7:0] tiny_ext,
    output logic [15:0] mid_ext,
    output logic [63:0] quad_ext,
    output logic        all_ones
);
  assign half_out = half_in + 16'd1;
  assign quad_out = {8'hA5, count};
  assign wide_out = ~wide_in;
  assign tiny_ext = 8'(tiny_in);
  assign mid_ext  = 16'(mid_in);
  assign quad_ext = 64'(quad_in);
  assign all_ones = &tiny_in && &mid_in && &quad_in;

  always_ff @(posedge clk) begin
    if (rst) count <= '0;
    else count <= count + 32'd1;
    case (cmd)
      8'd1: $fatal(1, "counter fatal at count=%0d", count);
      8'd2: $error("counter error at count=%0d", count);
      8'd3: $finish;
      8'd4: $display("count=%0d", count);
      8'd5: $stop;
      default: ;
    endcase
  end
endmodule
