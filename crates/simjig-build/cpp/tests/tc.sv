// Test design for the trace, coverage and isolation tests (cpp/tests/main.cpp).
// Verilated twice: as Vtc_full (--trace-fst --coverage) and Vtc_plain (neither).
module tc (
    input  logic        clk,
    input  logic        rst,
    output logic [7:0]  cnt,
    output logic [31:0] rnd,
    output logic        odd
);
  tc_sub u (.clk(clk), .d(cnt[0]), .q(odd));
  always_ff @(posedge clk) begin
    if (rst) begin
      cnt <= 8'd0;
      rnd <= 32'd0;
    end else begin
      cnt <= cnt + 8'd1;
      rnd <= $urandom;
      if (cnt == 8'd3) $display("tc %m cnt=%0d t=%0t", cnt, $time);
    end
  end
endmodule

module tc_sub (
    input  logic clk,
    input  logic d,
    output logic q
);
  logic r;
  always_ff @(posedge clk) begin
    r <= d;
    q <= r;
  end
endmodule
