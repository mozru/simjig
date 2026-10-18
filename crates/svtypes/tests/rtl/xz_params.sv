// Constants with X or Z bits, which the two-state IR cannot hold. None of them reaches a
// port or a type, so the design must still decode. Kept outside `tests/fixtures`, whose
// cases every IR consumer snapshots.
package xz_pkg;
  localparam logic [3:0] XV = 4'b10x1;
  localparam logic [3:0] ZV = 4'bzz00;
  localparam logic [3:0] OK = 4'b1001;
endpackage

module xz_params #(parameter logic [1:0] P = 2'bx1) (
  input  logic [3:0] a,
  output logic [3:0] b
);
  assign b = a ^ xz_pkg::OK;
endmodule
