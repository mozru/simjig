// One pipeline stage of tagged beats that counts hops: each beat on `up` leaves on `down`
// one cycle later, its `data` unchanged and its `tag` (the number of stages it has
// crossed) incremented, wrapping at 2**TAG_W. `rst` (synchronous, active high) empties
// the stage.
module hop_stage #(
    parameter int WIDTH = 8,
    parameter int TAG_W = 2
) (
    input logic      clk,
    input logic      rst,
    tagged_if.sink   up,
    tagged_if.source down
);
  timeunit 1ns; timeprecision 1ps;

  // An interface port carries no parameters (IEEE 1800-2017 §25.5): each interface keeps
  // its own WIDTH and TAG_W, which simjig's binding defaults to the module's `.param`s.
  if ($bits(up.beat.data) != WIDTH || $bits(up.beat.tag) != TAG_W
      || $bits(down.beat) != $bits(up.beat)) begin : g_bad_iface_width
    $fatal(1, "hop_stage: WIDTH %0d and TAG_W %0d disagree with the interfaces' beats",
           WIDTH, TAG_W);
  end

  always_ff @(posedge clk) begin
    if (rst) begin
      down.valid <= 1'b0;
      down.beat  <= '0;
    end else begin
      down.valid     <= up.valid;
      down.beat.data <= up.beat.data;
      down.beat.tag  <= up.beat.tag + 1'b1;
    end
  end
endmodule
