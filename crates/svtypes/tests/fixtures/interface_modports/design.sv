// An interface with modports, used by modules whose interface ports are connected inside
// a wrapper top (Verilator rejects interface ports on the top module).
interface bus_if #(parameter int W = 8) (input logic clk);
  logic [W-1:0] data;
  logic         valid;
  logic         ready;
  modport src (output data, output valid, input ready, input clk);
  modport snk (input data, input valid, output ready, input clk);
endinterface

module producer (bus_if.src bus, input logic go);
  assign bus.valid = go;
  assign bus.data  = '1;
endmodule

module consumer (bus_if.snk bus, output logic seen);
  assign bus.ready = 1'b1;
  assign seen = bus.valid;
endmodule

module monitor (bus_if bus, output logic busy);
  assign busy = bus.valid & ~bus.ready;
endmodule

module top (input logic clk, input logic go, output logic seen, output logic busy);
  bus_if #(.W(16)) bus (.clk(clk));
  producer u_prod (.bus(bus), .go(go));
  consumer u_cons (.bus(bus), .seen(seen));
  monitor  u_mon  (.bus(bus), .busy(busy));
endmodule
