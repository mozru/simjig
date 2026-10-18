// Raises one of $fatal, $stop, $error or $finish, chosen either by inputs at run time or
// by the AT_START parameter at time 0.
//   kind: 0 = $fatal, 1 = $stop, 2 = $error, 3 = $finish.
//   Run time: on posedge clk with fire == 1, the task selected by `kind` runs once.
//   AT_START: -1 (default) does nothing; 0..3 runs that task from an initial block.
// `alive` counts clk edges (cleared by rst) so a testbench can tell whether the model
// kept evaluating.
module fatal #(
    parameter int AT_START = -1
) (
    input  logic       clk,
    input  logic       rst,
    input  logic       fire,
    input  logic [1:0] kind,
    output logic [7:0] alive
);
  initial begin
    case (AT_START)
      0: $fatal(1, "fatal: AT_START $fatal");
      1: $stop;
      2: $error("fatal: AT_START $error");
      3: $finish;
      default: ;
    endcase
  end

  always_ff @(posedge clk) begin
    alive <= rst ? 8'd0 : alive + 8'd1;
    if (fire && !rst) begin
      case (kind)
        2'd0: $fatal(1, "fatal: kind=0 $fatal");
        2'd1: $stop;
        2'd2: $error("fatal: kind=2 $error");
        2'd3: $finish;
      endcase
    end
  end
endmodule
