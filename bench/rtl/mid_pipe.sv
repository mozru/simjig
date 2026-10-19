// mid_pipe: the benchmark's mid-size design (bench/README.md, "Designs"). A 64-bit
// sync_fifo (DEPTH entries, registered read) feeding a STAGES-deep processing pipeline:
// enough flops and combinational logic per cycle that Verilator's eval is a real part of
// a testbench's time, behind the same small push/pop port as the tiny FIFO benchmark.
//
// Pipeline. Every entry a pop takes leaves the FIFO on `rd_data` and enters stage 0 at
// the next edge. Stages come in pairs: the even stage of pair j mixes its input,
//   y = rotl((x ^ (x >> 32)) * K_j, R_j),
// and the odd stage undoes it,
//   a = rotr(y, R_j) * K_j^-1;  x = a ^ (a >> 32),
// where K_j is odd and K_j^-1 its inverse mod 2^64 (`x ^ (x >> 32)` is its own inverse).
// So `out_data` is the popped entry again, which a testbench checks for the price of a
// queue, while each stage still costs Verilator a 64-bit multiply (Verilator does no
// algebra across registers, so nothing cancels). Alongside the data, each stage folds
// its input into a CRC-32 (reflected, polynomial 0xEDB88320, bit-serial over 64 bits:
// 64 dependent steps of combinational depth per stage), so `out_crc` depends on every
// intermediate value; a testbench folds it into its result hash.
//
// Timing. A pop taken at an edge (`pop && !empty`) shows the entry on `rd_data` after
// that edge; it leaves stage STAGES-1 STAGES edges later. Counting a testbench's samples
// just before each rising edge, an entry popped at sample i is on `out_data`, with
// `out_valid` high, at sample i + STAGES + 1 (and `out_valid` is low at every sample
// that no pop is due at).
//
// Reset (`rst`, synchronous, active high) empties the FIFO and clears every stage's
// valid bit; data and CRC registers are not reset (they mean nothing while not valid).
module mid_pipe #(
    parameter int STAGES = 128,  // even, at least 2
    parameter int DEPTH  = 256  // a power of two (sync_fifo's rule)
) (
    input  logic                   clk,
    input  logic                   rst,
    input  logic                   push,
    input  logic [63:0]            wr_data,
    input  logic                   pop,
    output logic                   full,
    output logic                   empty,
    output logic [$clog2(DEPTH):0] count,
    output logic                   out_valid,
    output logic [63:0]            out_data,
    output logic [31:0]            out_crc
);
  timeunit 1ns; timeprecision 1ps;

  if (STAGES < 2 || STAGES % 2 != 0) begin : g_bad_stages
    $fatal(1, "mid_pipe: STAGES must be even, at least 2 (got %0d)", STAGES);
  end

  // The multiplier of pair j: odd, and different for every pair.
  function automatic logic [63:0] mul_k(int j);
    return (64'h9E37_79B9_7F4A_7C15 * 64'(2 * j + 1)) | 64'd1;
  endfunction

  // The inverse of an odd c mod 2^64, by Newton's iteration x <- x * (2 - c * x): an odd
  // c is its own inverse mod 8 (3 correct bits), and each step doubles them (3 to 96).
  function automatic logic [63:0] inv64(logic [63:0] c);
    logic [63:0] x = c;
    for (int i = 0; i < 5; i++) x = x * (64'd2 - c * x);
    return x;
  endfunction

  function automatic logic [31:0] crc32_64(logic [31:0] crc, logic [63:0] data);
    logic [31:0] c = crc;
    for (int i = 0; i < 64; i++) c = (c >> 1) ^ (32'hEDB8_8320 & {32{c[0] ^ data[i]}});
    return c;
  endfunction

  logic [63:0] rd_data;
  sync_fifo #(
      .WIDTH(64),
      .DEPTH(DEPTH),
      .FWFT (1'b0)
  ) u_fifo (
      .clk,
      .rst,
      .push,
      .wr_data,
      .pop,
      .rd_data,
      .full,
      .empty,
      .count
  );

  // Whether `rd_data` holds an entry popped at the last edge: stage 0's valid input.
  logic popped;
  always_ff @(posedge clk) popped <= !rst && pop && !empty;

  // Stage k reads v[k], d[k], c[k] and drives v[k+1], d[k+1], c[k+1].
  logic        v[STAGES+1];
  logic [63:0] d[STAGES+1];
  logic [31:0] c[STAGES+1];
  assign v[0] = popped;
  assign d[0] = rd_data;
  assign c[0] = 32'hFFFF_FFFF;

  for (genvar k = 0; k < STAGES; k++) begin : g_stage
    localparam logic [63:0] K = mul_k(k / 2);
    localparam logic [63:0] KINV = inv64(K);
    localparam int R = 7 + 2 * (k / 2) % 50;  // 7..55
    logic [63:0] y;
    if (k % 2 == 0) begin : g_mix
      wire [63:0] a = (d[k] ^ (d[k] >> 32)) * K;
      assign y = (a << R) | (a >> (64 - R));
    end else begin : g_unmix
      wire [63:0] a = ((d[k] >> R) | (d[k] << (64 - R))) * KINV;
      assign y = a ^ (a >> 32);
    end
    logic        v_q;
    logic [63:0] d_q;
    logic [31:0] c_q;
    always_ff @(posedge clk) begin
      v_q <= !rst && v[k];
      d_q <= y;
      c_q <= crc32_64(c[k], d[k]);
    end
    assign v[k+1] = v_q;
    assign d[k+1] = d_q;
    assign c[k+1] = c_q;
  end

  assign out_valid = v[STAGES];
  assign out_data  = d[STAGES];
  assign out_crc   = c[STAGES];
endmodule
