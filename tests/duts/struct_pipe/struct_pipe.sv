// Two-stage pipeline over a packed struct. Stage 1 registers the input beat; stage 2
// applies the beat's op to its payload with `operand` and registers the result. The tag
// and op pass through unchanged. Latency is two cycles; there is no back-pressure.
module struct_pipe
  import struct_pipe_pkg::*;
(
    input  logic                 clk,
    input  logic                 rst,
    input  logic                 in_valid,
    input  beat_t                in_beat,
    input  logic [PAYLOAD_W-1:0] operand,
    output logic                 out_valid,
    output beat_t                out_beat
);
  beat_t s1_beat;
  logic  s1_valid;

  function automatic logic [PAYLOAD_W-1:0] apply(op_e op, logic [PAYLOAD_W-1:0] x,
                                                 logic [PAYLOAD_W-1:0] y);
    case (op)
      OP_ADD:  return x + y;
      OP_XOR:  return x ^ y;
      OP_INV:  return ~x;
      default: return x;
    endcase
  endfunction

  always_ff @(posedge clk) begin
    if (rst) begin
      s1_valid  <= 1'b0;
      out_valid <= 1'b0;
      s1_beat   <= '0;
      out_beat  <= '0;
    end else begin
      s1_valid         <= in_valid;
      s1_beat          <= in_beat;
      out_valid        <= s1_valid;
      out_beat.op      <= s1_beat.op;
      out_beat.tag     <= s1_beat.tag;
      out_beat.payload <= apply(s1_beat.op, s1_beat.payload, operand);
    end
  end
endmodule
