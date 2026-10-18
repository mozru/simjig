// A dual-clock FIFO: Gray-code pointers crossed through 2-flop synchronizers
// (C. E. Cummings, "Simulation and Synthesis Techniques for Asynchronous FIFO Design",
// SNUG 2002, style #1).
//
// Each pointer is one bit wider than the address, so `wptr == rptr` is empty and the
// pointers differing only in their top two Gray bits is full. A Gray pointer changes one
// bit per increment, so a synchronizer that samples it mid-change sees either the old or
// the new value, never a mix. The crossed pointer is therefore stale but never wrong:
// `wfull` and `rempty` are pessimistic (they may stay asserted a few cycles after the
// other side made room or wrote) and never optimistic.
//
// Write side (`wclk`): a write happens at a rising edge with `winc && !wfull`.
// Read side (`rclk`): `rdata` shows the oldest entry while `!rempty` (first-word
// fall-through); `rinc && !rempty` at a rising edge pops it.
// Each side has its own asynchronous, active-low reset; release each synchronously to its
// own clock.
module async_fifo #(
    parameter int unsigned WIDTH = 8,
    parameter int unsigned DEPTH = 16  // a power of 2, at least 2
) (
    input  logic             wclk,
    input  logic             wrst_n,
    input  logic             winc,
    input  logic [WIDTH-1:0] wdata,
    output logic             wfull,

    input  logic             rclk,
    input  logic             rrst_n,
    input  logic             rinc,
    output logic [WIDTH-1:0] rdata,
    output logic             rempty
);
  timeunit 1ns; timeprecision 1ps;

  localparam int unsigned AW = $clog2(DEPTH);
  typedef logic [AW:0] ptr_t;
  // Full: the write pointer is one lap ahead, which in Gray code inverts the top two bits.
  localparam ptr_t FULL_FLIP = ptr_t'(3) << (AW - 1);

  if (DEPTH < 2 || (1 << AW) != DEPTH) begin : g_check_depth
    $error("async_fifo: DEPTH must be a power of 2 and at least 2, got %0d", DEPTH);
  end

  function automatic ptr_t bin2gray(ptr_t b);
    return b ^ (b >> 1);
  endfunction

  logic [WIDTH-1:0] mem[DEPTH];

  // ---- write domain ----
  ptr_t wbin, wgray, wbin_next, wgray_next;
  ptr_t wq1_rgray, wq2_rgray;  // the read pointer, synchronized into wclk

  assign wbin_next  = wbin + ptr_t'(winc && !wfull);
  assign wgray_next = bin2gray(wbin_next);

  always_ff @(posedge wclk or negedge wrst_n) begin
    if (!wrst_n) begin
      wbin <= '0;
      wgray <= '0;
      wfull <= 1'b0;
      {wq2_rgray, wq1_rgray} <= '0;
    end else begin
      wbin <= wbin_next;
      wgray <= wgray_next;
      wfull <= wgray_next == (wq2_rgray ^ FULL_FLIP);
      {wq2_rgray, wq1_rgray} <= {wq1_rgray, rgray};
    end
  end

  always_ff @(posedge wclk) begin
    if (winc && !wfull) mem[wbin[AW-1:0]] <= wdata;
  end

  // ---- read domain ----
  ptr_t rbin, rgray, rbin_next, rgray_next;
  ptr_t rq1_wgray, rq2_wgray;  // the write pointer, synchronized into rclk

  assign rbin_next  = rbin + ptr_t'(rinc && !rempty);
  assign rgray_next = bin2gray(rbin_next);
  assign rdata      = mem[rbin[AW-1:0]];

  always_ff @(posedge rclk or negedge rrst_n) begin
    if (!rrst_n) begin
      rbin <= '0;
      rgray <= '0;
      rempty <= 1'b1;
      {rq2_wgray, rq1_wgray} <= '0;
    end else begin
      rbin <= rbin_next;
      rgray <= rgray_next;
      rempty <= rgray_next == rq2_wgray;
      {rq2_wgray, rq1_wgray} <= {rq1_wgray, wgray};
    end
  end
endmodule
