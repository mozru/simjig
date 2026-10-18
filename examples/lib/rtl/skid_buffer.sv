// Skid buffer: a valid/ready pipeline register that cuts every combinational path between
// its two sides. `down.valid`, `down.data` and `up.ready` all come straight from flops,
// and with `down.ready` held high it moves one beat per cycle with one cycle of latency.
//
// Two entries: the output register (`out_*`) and the skid register (`skid_*`). The skid
// catches the one beat that `up` may send in the cycle `down` stalls, because `up.ready`
// is registered and cannot drop in that same cycle. `up.ready` is `!skid_valid`.
//
// Each posedge, with `load = !out_valid || down.ready` (the output register is free):
// - load, skid full:  skid -> out (up.ready was low, so nothing is taken from up);
// - load, skid empty: up -> out when up.valid, else out empties;
// - no load: a beat taken from up (up.valid && up.ready) goes into the skid.
//
// Reset (`rst`, synchronous, active high) empties both registers and beats the load and
// the skid write at the same edge. `up.ready` is not gated by `rst`: from the first reset
// edge on, the skid is empty, so `up.ready` reads high while `rst` is still high, and a
// beat `up` offers at a reset edge looks taken but is discarded. A source must therefore
// be reset with the buffer (the same `rst`) or ignore handshakes while `rst` is high.
// Gating `up.ready` with `rst` would hide this, at the cost of a combinational path from
// `rst` to an output that every other path here avoids.
module skid_buffer #(
    parameter int WIDTH = 8
) (
    input logic      clk,
    input logic      rst,
    stream_if.sink   up,
    stream_if.source down
);
  timeunit 1ns; timeprecision 1ps;

  if (WIDTH < 1) begin : g_bad_width
    $fatal(1, "skid_buffer: WIDTH must be at least 1 (got %0d)", WIDTH);
  end
  // Each interface instance has its own WIDTH parameter, and nothing in the language ties
  // it to this module's: an interface port header carries no parameter assignments (IEEE
  // 1800-2017 §25.5). simjig's binding defaults it to the module's `.param("WIDTH", ..)`,
  // but another instantiation may not; a mismatch would silently truncate or pad every
  // beat.
  if ($bits(up.data) != WIDTH || $bits(down.data) != WIDTH) begin : g_bad_iface_width
    $fatal(1, "skid_buffer: WIDTH is %0d but up.data has %0d bits and down.data %0d",
           WIDTH, $bits(up.data), $bits(down.data));
  end

  logic             out_valid, skid_valid;
  logic [WIDTH-1:0] out_data, skid_data;

  wire load = !out_valid || down.ready;

  assign up.ready   = !skid_valid;
  assign down.valid = out_valid;
  assign down.data  = out_data;

  always_ff @(posedge clk) begin
    if (rst) begin
      out_valid  <= 1'b0;
      skid_valid <= 1'b0;
    end else if (load) begin
      out_valid  <= skid_valid || up.valid;
      skid_valid <= 1'b0;
    end else if (up.valid && up.ready) begin
      skid_valid <= 1'b1;
    end
  end

  // Data registers are not reset: each is read only while its valid is set.
  always_ff @(posedge clk) begin
    if (load) begin
      if (skid_valid) out_data <= skid_data;
      else if (up.valid) out_data <= up.data;
    end else if (up.valid && up.ready) begin
      skid_data <= up.data;
    end
  end
endmodule
