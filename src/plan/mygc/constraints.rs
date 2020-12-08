pub use crate::plan::plan_constraints::*;

// It's a copying collector, so it moves objects
pub const MOVES_OBJECTS: bool = true;
pub const GC_HEADER_BITS: usize = 2;
pub const GC_HEADER_WORDS: usize = 0;
pub const NUM_SPECIALIZED_SCANS: usize = 1;