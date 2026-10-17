// Top whose data ports are interface modports, so it cannot be a Verilator top as-is: a
// wrapper must instantiate bus_if and bind it (DECISION.md table 2, row 9).
// One-entry skid-free register slice: a beat accepted on `in_bus` appears on `out_bus` on
// the next cycle, with `data` incremented by one. `in_bus.ready` is high whenever the
// slice is empty or its beat is leaving.
module iface_top
  import iface_pkg::*;
(
    input logic   clk,
    input logic   rst,
    bus_if.dst    in_bus,
    bus_if.src    out_bus
);
  logic              full;
  logic [DATA_W-1:0] data_q;
  kind_e             kind_q;

  assign in_bus.ready  = !full || out_bus.ready;
  assign out_bus.valid = full;
  assign out_bus.data  = data_q;
  assign out_bus.kind  = kind_q;

  always_ff @(posedge clk) begin
    if (rst) begin
      full   <= 1'b0;
      data_q <= '0;
      kind_q <= KIND_IDLE;
    end else if (in_bus.ready) begin
      full <= in_bus.valid;
      if (in_bus.valid) begin
        data_q <= in_bus.data + 1'b1;
        kind_q <= in_bus.kind;
      end
    end
  end
endmodule
