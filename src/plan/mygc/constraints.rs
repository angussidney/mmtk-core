pub use crate::plan::plan_constraints::*;

// NoGC doesn't have any mark bits or anything added to each object
pub const GC_HEADER_BITS: usize = 0;
pub const GC_HEADER_WORDS: usize = 0;
