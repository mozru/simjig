// Spike model: `sel` picks which system task runs on the next rising clock edge.
module fat(input logic clk, input logic [7:0] sel, output logic [31:0] cnt);
  logic [7:0] mem [0:3];
  always_ff @(posedge clk) begin
    cnt <= cnt + 1;
    case (sel)
      1: begin $fatal(1, "boom %0d", cnt); $display("after fatal"); end
      2: $stop;
      3: $error("err %0d", cnt);
      4: $finish;
      5: $display("hello %0d", cnt);
      6: begin $write("part1 "); $write("part2\n"); end
      7: $warning("warn %0d", cnt);
      8: $readmemh("bad.hex", mem);
      default: ;
    endcase
  end
endmodule
