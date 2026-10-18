// Synchronous FIFO: one clock, synchronous active-high reset.
//
// Parameters:
// - WIDTH: bits per entry (at least 1).
// - DEPTH: entries; a power of two, at least 2. The pointers carry one extra wrap bit, so
//   `count = wr_ptr - rd_ptr` holds 0..DEPTH without a separate counter, and full and
//   empty are both `rd_idx == wr_idx`, told apart by the wrap bits. Any other DEPTH stops
//   elaboration with `$fatal` (IEEE 1800-2023 §20.11), which Verilator 5.052 reports as
//   a USERFATAL warning: fatal unless the build passes `-Wno-fatal`.
// - FWFT: first-word fall-through. 1: `rd_data` shows the head entry whenever `!empty`,
//   and `pop` consumes it. 0: `rd_data` is a register loaded by a pop, so the entry a pop
//   removes appears on `rd_data` after that clock edge and holds until the next pop.
//
// Handshake, sampled at posedge clk:
// - a push is taken when `push && (!full || pop)`: a full FIFO takes a push in the same
//   cycle as a pop, and drops it otherwise;
// - a pop is taken when `pop && !empty`: an empty FIFO ignores it, even with a push in
//   the same cycle (an entry is never pushed and popped in one cycle).
// `count`, `full` and `empty` are registered state, never combinational from push/pop.
module sync_fifo #(
    parameter int WIDTH = 8,
    parameter int DEPTH = 4,
    parameter bit FWFT  = 1'b0
) (
    input  logic                   clk,
    input  logic                   rst,
    input  logic                   push,
    input  logic [WIDTH-1:0]       wr_data,
    input  logic                   pop,
    output logic [WIDTH-1:0]       rd_data,
    output logic                   full,
    output logic                   empty,
    output logic [$clog2(DEPTH):0] count
);
  localparam int AW = $clog2(DEPTH);

  if (DEPTH < 2 || (DEPTH & (DEPTH - 1)) != 0) begin : g_bad_depth
    $fatal(1, "sync_fifo: DEPTH must be a power of two, at least 2 (got %0d)", DEPTH);
  end
  if (WIDTH < 1) begin : g_bad_width
    $fatal(1, "sync_fifo: WIDTH must be at least 1 (got %0d)", WIDTH);
  end

  logic [WIDTH-1:0] mem[DEPTH];
  logic [AW:0] wr_ptr, rd_ptr;
  wire [AW-1:0] wr_idx = wr_ptr[AW-1:0];
  wire [AW-1:0] rd_idx = rd_ptr[AW-1:0];

  assign count = wr_ptr - rd_ptr;
  assign empty = wr_ptr == rd_ptr;
  assign full  = wr_idx == rd_idx && wr_ptr[AW] != rd_ptr[AW];

  wire do_pop  = pop && !empty;
  wire do_push = push && (!full || do_pop);

  always_ff @(posedge clk) begin
    if (rst) begin
      wr_ptr <= '0;
      rd_ptr <= '0;
    end else begin
      if (do_push) wr_ptr <= wr_ptr + 1'b1;
      if (do_pop) rd_ptr <= rd_ptr + 1'b1;
    end
  end

  // Storage is not reset: a slot is read only after a push wrote it.
  always_ff @(posedge clk) begin
    if (do_push) mem[wr_idx] <= wr_data;
  end

  if (FWFT) begin : g_fwft
    assign rd_data = mem[rd_idx];
  end else begin : g_registered
    logic [WIDTH-1:0] rd_q;
    // Nonblocking: a pop of the slot a simultaneous push (on a full FIFO) overwrites
    // reads the old entry.
    always_ff @(posedge clk) begin
      if (rst) rd_q <= '0;
      else if (do_pop) rd_q <= mem[rd_idx];
    end
    assign rd_data = rd_q;
  end
endmodule
