//! A plain indenting text writer, shared by every emitter (DECISION.md table 2, row 5).
//!
//! Output is a pure function of the calls: `\n` line ends, no trailing whitespace, and
//! blank lines never indented, so emitted files are byte-stable.

/// Accumulates indented lines.
#[derive(Debug)]
pub(crate) struct Writer {
    out: String,
    depth: usize,
    unit: &'static str,
}

impl Writer {
    /// A writer that indents by `unit` per level.
    pub(crate) fn new(unit: &'static str) -> Self {
        Self {
            out: String::new(),
            depth: 0,
            unit,
        }
    }

    /// Writes one line at the current depth; an empty `text` is a blank line.
    pub(crate) fn line(&mut self, text: impl AsRef<str>) {
        let text = text.as_ref();
        if !text.is_empty() {
            for _ in 0..self.depth {
                self.out.push_str(self.unit);
            }
            self.out.push_str(text);
        }
        self.out.push('\n');
    }

    /// Writes `text` and indents the following lines.
    pub(crate) fn open(&mut self, text: impl AsRef<str>) {
        self.line(text);
        self.depth += 1;
    }

    /// Dedents, then writes `text`.
    pub(crate) fn close(&mut self, text: impl AsRef<str>) {
        self.depth = self.depth.saturating_sub(1);
        self.line(text);
    }

    /// The text written so far.
    pub(crate) fn finish(self) -> String {
        self.out
    }
}

#[cfg(test)]
mod tests {
    use super::Writer;

    #[test]
    fn indents_and_keeps_blank_lines_bare() {
        let mut w = Writer::new("  ");
        w.open("a {");
        w.line("b;");
        w.line("");
        w.close("}");
        assert_eq!(w.finish(), "a {\n  b;\n\n}\n");
    }
}
