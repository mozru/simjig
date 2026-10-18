// Dual-clock FIFO with Gray-coded pointers and two-flop synchronizers (Cummings style).
// DEPTH must be a power of two, at least 4. The write side runs on wclk, the read side on
// rclk; the clocks are unrelated. Each reset is synchronous to its own clock, and both
// must be asserted together for at least two cycles of the slower clock.
// The read data is first-word fall-through: rdata is valid whenever !rempty.
module async_fifo #(
    parameter int WIDTH = 8,
    parameter int DEPTH = 8
) (
    input  logic             wclk,
    input  logic             wrst,
    input  logic             winc,
    input  logic [WIDTH-1:0] wdata,
    output logic             wfull,

    input  logic             rclk,
    input  logic             rrst,
    input  logic             rinc,
    output logic [WIDTH-1:0] rdata,
    output logic             rempty
);
  localparam int AW = $clog2(DEPTH);

  logic [WIDTH-1:0] mem[DEPTH];

  logic [AW:0] wbin, wgray, rbin, rgray;
  logic [AW:0] rgray_w1, rgray_w2;  // read pointer synchronized into wclk
  logic [AW:0] wgray_r1, wgray_r2;  // write pointer synchronized into rclk

  function automatic logic [AW:0] bin2gray(logic [AW:0] b);
    return b ^ (b >> 1);
  endfunction

  // Write domain.
  wire [AW:0] wbin_next = wbin + (AW + 1)'(winc && !wfull);
  wire [AW:0] wgray_next = bin2gray(wbin_next);
  always_ff @(posedge wclk) begin
    if (wrst) begin
      wbin     <= '0;
      wgray    <= '0;
      rgray_w1 <= '0;
      rgray_w2 <= '0;
      wfull    <= 1'b0;
    end else begin
      if (winc && !wfull) mem[wbin[AW-1:0]] <= wdata;
      wbin     <= wbin_next;
      wgray    <= wgray_next;
      rgray_w1 <= rgray;
      rgray_w2 <= rgray_w1;
      // Full when the next write pointer equals the read pointer with the top two bits
      // inverted (Gray-code wrap).
      wfull    <= wgray_next == {~rgray_w2[AW:AW-1], rgray_w2[AW-2:0]};
    end
  end

  // Read domain.
  wire [AW:0] rbin_next = rbin + (AW + 1)'(rinc && !rempty);
  wire [AW:0] rgray_next = bin2gray(rbin_next);
  always_ff @(posedge rclk) begin
    if (rrst) begin
      rbin     <= '0;
      rgray    <= '0;
      wgray_r1 <= '0;
      wgray_r2 <= '0;
      rempty   <= 1'b1;
    end else begin
      rbin     <= rbin_next;
      rgray    <= rgray_next;
      wgray_r1 <= wgray;
      wgray_r2 <= wgray_r1;
      rempty   <= rgray_next == wgray_r2;
    end
  end

  assign rdata = mem[rbin[AW-1:0]];
endmodule
