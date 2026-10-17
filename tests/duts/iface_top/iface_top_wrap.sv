// Reference wrapper: flattens iface_top's interface ports into plain ports so the design
// can be a Verilator top (Verilator rejects interface ports on the top module). This is
// the shape simjig-build's generated wrapper must take (DECISION.md table 2, row 9); the
// smoke test in tests/duts/smoke uses it until the generator exists.
module iface_top_wrap
  import iface_pkg::*;
(
    input  logic              clk,
    input  logic              rst,
    input  logic              in_valid,
    output logic              in_ready,
    input  logic [DATA_W-1:0] in_data,
    input  kind_e             in_kind,
    output logic              out_valid,
    input  logic              out_ready,
    output logic [DATA_W-1:0] out_data,
    output kind_e             out_kind
);
  bus_if in_bus ();
  bus_if out_bus ();

  assign in_bus.valid  = in_valid;
  assign in_bus.data   = in_data;
  assign in_bus.kind   = in_kind;
  assign in_ready      = in_bus.ready;
  assign out_valid     = out_bus.valid;
  assign out_data      = out_bus.data;
  assign out_kind      = out_bus.kind;
  assign out_bus.ready = out_ready;

  iface_top dut (
      .clk(clk),
      .rst(rst),
      .in_bus(in_bus.dst),
      .out_bus(out_bus.src)
  );
endmodule
