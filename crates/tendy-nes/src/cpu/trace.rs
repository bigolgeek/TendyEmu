use super::Cpu;

impl Cpu {
    /// The state before the next instruction, e.g. "C000 A:00 X:00 Y:00 P:24 SP:FD CYC:7".
    /// Hex fields are upper case and zero-padded (pc 4 digits, registers 2 digits); CYC is decimal.
    pub fn trace_line(&self) -> String {
        format!(
            "{:04X} A:{:02X} X:{:02X} Y:{:02X} P:{:02X} SP:{:02X} CYC:{}",
            self.pc, self.a, self.x, self.y, self.p, self.sp, self.cycles
        )
    }
}
