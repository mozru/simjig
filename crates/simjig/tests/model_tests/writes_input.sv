// A design that writes its own input, which Verilator allows once its ASSIGNIN lint is
// waived: an eval then changes an input (tests/model_tests/walks.rs).
module writes_input (
    input  logic clk,
    input  logic a,
    output logic q
);
  /* verilator lint_off ASSIGNIN */
  always_ff @(posedge clk) a <= ~a;
  /* verilator lint_on ASSIGNIN */
  assign q = a;
endmodule
