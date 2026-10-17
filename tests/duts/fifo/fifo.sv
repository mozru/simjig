// Synchronous valid/ready FIFO. DEPTH must be a power of two, at least 2.
// A beat moves on the input side when in_valid && in_ready, and on the output side when
// out_valid && out_ready, both sampled at posedge clk. out_data is registered storage read
// combinationally at the read pointer (first-word fall-through).
module fifo #(
    parameter int WIDTH = 8,
    parameter int DEPTH = 4
) (
    input  logic             clk,
    input  logic             rst,
    input  logic             in_valid,
    output logic             in_ready,
    input  logic [WIDTH-1:0] in_data,
    output logic             out_valid,
    input  logic             out_ready,
    output logic [WIDTH-1:0] out_data,
    output logic [$clog2(DEPTH):0] level
);
  localparam int AW = $clog2(DEPTH);

  logic [WIDTH-1:0] mem[DEPTH];
  logic [AW:0] wr_ptr, rd_ptr;

  wire push = in_valid && in_ready;
  wire pop = out_valid && out_ready;

  assign level     = wr_ptr - rd_ptr;
  assign in_ready  = level != DEPTH[AW:0];
  assign out_valid = level != '0;
  assign out_data  = mem[rd_ptr[AW-1:0]];

  always_ff @(posedge clk) begin
    if (rst) begin
      wr_ptr <= '0;
      rd_ptr <= '0;
    end else begin
      if (push) begin
        mem[wr_ptr[AW-1:0]] <= in_data;
        wr_ptr <= wr_ptr + 1'b1;
      end
      if (pop) rd_ptr <= rd_ptr + 1'b1;
    end
  end
endmodule
