use crate::plan::barriers::NoBarrier;
use crate::plan::mutator_context::Mutator;
use crate::plan::mutator_context::MutatorConfig;
use crate::plan::mygc::MyGC;
use crate::plan::AllocationSemantics as AllocationType;
use crate::util::alloc::allocators::{AllocatorSelector, Allocators};
use crate::util::OpaquePointer;
use crate::vm::VMBinding;
use enum_map::enum_map;
use enum_map::EnumMap;


// This code is only executed at runtime in order to be initialised
lazy_static! {
    // Map all possible allocation types to a simple bump pointer allocator
    pub static ref ALLOCATOR_MAPPING: EnumMap<AllocationType, AllocatorSelector> = enum_map! {
        AllocationType::Default | AllocationType::Immortal | AllocationType::Code | AllocationType::ReadOnly | AllocationType::Los => AllocatorSelector::BumpPointer(0),
    };
}

// Just a dummy unreachable function to pass as release/prepare func
pub fn nogc_mutator_noop<VM: VMBinding>(
    _mutator: &mut Mutator<MyGC<VM>>, // Mutator: per-thread data structure that manages allocations etc
    _tls: OpaquePointer // Thread local storage
) {
    unreachable!();
}

// Create mutator object
pub fn create_nogc_mutator<VM: VMBinding>(
    mutator_tls: OpaquePointer, // Thread local storage - doesn't actually have
        // to be storage, it only needs to be a unique pointer which MMTk can
        // use to identify the mutator
    plan: &'static MyGC<VM>, // Instance of the NoGC plan?
) -> Mutator<MyGC<VM>> {
    let config = MutatorConfig {
        allocator_mapping: &*ALLOCATOR_MAPPING, // Everything mapped to bump pointer allocator
        space_mapping: box vec![(AllocatorSelector::BumpPointer(0), &plan.nogc_space)],
        prepare_func: &nogc_mutator_noop, // we don't care about these
        release_func: &nogc_mutator_noop,
    };

    Mutator {
        // Types of allocators (bump pointer, large object etc)
        allocators: Allocators::<VM>::new(mutator_tls, plan, &config.space_mapping),
        barrier: box NoBarrier, // NoGC doesn't need to monitor read/writes
        mutator_tls,
        config,
        plan,
    }
}
