// Calls two imported DPI-C functions; dpi_impl.c holds a reference implementation.
//   sum:   combinational, dpi_add(a, b) (pure).
//   calls: registered count of dpi_tick() calls, one per posedge clk (context, side effect).
module dpi (
    input  logic        clk,
    input  logic [31:0] a,
    input  logic [31:0] b,
    output logic [31:0] sum,
    output logic [31:0] calls
);
  import "DPI-C" pure function int dpi_add(input int x, input int y);
  import "DPI-C" context function int dpi_tick();

  assign sum = dpi_add(a, b);

  always_ff @(posedge clk) calls <= dpi_tick();
endmodule
