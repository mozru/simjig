// A round-robin arbiter for N requesters with a rotating priority pointer.
//
// `gnt` is combinational: in each cycle it is one-hot on the first requester at or after
// the pointer `ptr` (wrapping at N), or zero when nothing requests. At each rising edge
// with a grant the pointer moves to the requester after the winner, so the winner has the
// lowest priority next cycle; a requester that holds its request is therefore granted
// within N cycles. While `hold` is high the pointer moves to the winner instead, so the
// current grantee keeps the grant for as long as it requests (a lock for multi-cycle
// transfers).
//
// Reset is synchronous and active low; it points the priority at requester 0.
module round_robin_arbiter #(
    parameter int unsigned N = 4
) (
    input  logic         clk,
    input  logic         rst_n,
    input  logic [N-1:0] req,
    input  logic         hold,
    output logic [N-1:0] gnt
);
  timeunit 1ns; timeprecision 1ps;

  localparam int unsigned IW = (N > 1) ? $clog2(N) : 1;
  typedef logic [IW-1:0] idx_t;

  if (N < 2) begin : g_check_n
    $fatal(1, "round_robin_arbiter: N must be at least 2, got %0d", N);
  end

  idx_t ptr;     // the requester with the highest priority this cycle
  idx_t winner;  // the requester granted this cycle (meaningful when `granted`)
  logic granted;
  int unsigned cand;

  // Scan the N requesters starting at the pointer; the first one requesting wins.
  always_comb begin
    gnt = '0;
    winner = ptr;
    granted = 1'b0;
    cand = 0;
    for (int unsigned i = 0; i < N; i++) begin
      cand = 32'(ptr) + i;
      if (cand >= N) cand = cand - N;
      if (!granted && req[idx_t'(cand)]) begin
        granted = 1'b1;
        winner = idx_t'(cand);
        gnt[idx_t'(cand)] = 1'b1;
      end
    end
  end

  always_ff @(posedge clk) begin
    if (!rst_n) ptr <= '0;
    else if (granted) begin
      if (hold) ptr <= winner;
      else if (winner == idx_t'(N - 1)) ptr <= '0;
      else ptr <= winner + idx_t'(1);
    end
  end
endmodule
