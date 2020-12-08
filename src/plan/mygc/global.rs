use super::gc_works::{SSCopyContext, SSProcessEdges};
use crate::mmtk::MMTK;
use crate::plan::global::{BasePlan, NoCopy};
use crate::plan::global::CommonPlan;
use crate::plan::global::GcStatus;
use crate::plan::mutator_context::Mutator;
use crate::plan::mygc::mutator::create_ss_mutator;
use crate::plan::mygc::mutator::ALLOCATOR_MAPPING;
use crate::plan::AllocationSemantics;
use crate::plan::Plan;
use crate::policy::copyspace::CopySpace;
use crate::policy::space::Space;
use crate::scheduler::gc_works::*;
use crate::scheduler::*;
use crate::util::alloc::allocators::AllocatorSelector;
use crate::util::heap::layout::heap_layout::Mmapper;
use crate::util::heap::layout::heap_layout::VMMap;
use crate::util::heap::layout::vm_layout_constants::{HEAP_END, HEAP_START};
use crate::util::heap::HeapMeta;
#[allow(unused_imports)]
use crate::util::heap::VMRequest;
use crate::util::options::UnsafeOptionsWrapper;
#[cfg(feature = "sanity")]
use crate::util::sanity::sanity_checker::*;
use crate::util::OpaquePointer;
use crate::vm::VMBinding;
use enum_map::EnumMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

pub type SelectedPlan<VM> = MyGC<VM>;

// Says that for the semispace algorithm we should use the default allocation
// method i.e. to the copyspaces. This is as opposed to putting stuff in
// the large object space, immortal space, readonly etc
pub const ALLOC_SS: AllocationSemantics = AllocationSemantics::Default;
// Seems to be only used for debugging purposes internally?

// The base struct which represents the properties of our GC.
pub struct MyGC<VM: VMBinding> {
    pub hi: AtomicBool, // A thread-safe bool which determines which space
                        // is currently active
    pub copyspace0: CopySpace<VM>, // Two spaces: a from-space and to-space
    pub copyspace1: CopySpace<VM>, // Initially this is the from-space?
    pub common: CommonPlan<VM>, 
    // CommonPlan includes two additional spaces: (CommonUnsync)
    // - An immortal space, for objects that the VM (or another library)
    //   never expects to move
    // - A large object space.  This is necessary because the overhead of
    //   copying large objects can be quite high, so instead we manage them
    //   differently to everything else using a GC algorithm that doesn't
    //   actually copy the objects
    //   - One approach is to allocate each object to a single OS page,
    //     so that we can ask the OS to remap virtual memory in order to
    //     'copy' the large object. This is not used in MMTk.
    //   - Another approach is to use a Treadmill GC (https://dl.acm.org/doi/10.1145/130854.130862,
    //     GC handbook pg 138) which stores objects in a circular doubly
    //     linked list. This operates similarly to a generational semispace GC.
    //     To 'copy' objects, we simply update the pointers pointing to
    //     the object. Using pointers like this in large objects is fine
    //     because the space overhead is relatively small compared to the 
    //     objects, but for small objects the overhead would outweigh the
    //     benefits. Additionally, the inability to directly access a specific
    //     index is not really a problem, since there are relatively few large
    //     objects and so enumerating through the list is fast. Once again,
    //     this would not be the case for smaller objects
    //   - However, implementing this in Rust would be a pain because maintaining
    //     a doubly linked list requires lots of UNSAFE code, which is bad because
    //     it becomes harder to maintain the correctness of the program. As
    //     such, in MMTk the 'Treadmill' isn't actually a doubly linked list,
    //     instead we simply use a hash set. In this sense, this implementation
    //     is neither faithful to the original paper nor the old MMTk
    // A BasePlan is also included which is used to store basic properties 
    // that are common to **all** GCs.
}

unsafe impl<VM: VMBinding> Sync for MyGC<VM> {}

// Implement the Plan trait - this represents how this plan should be
// initialised, prepared, manually collected, statistically analyised etc
impl<VM: VMBinding> Plan for MyGC<VM> {
    type VM = VM;
    type Mutator = Mutator<Self>; // Mutator: per-thread data structure that manages allocations etc
    type CopyContext = SSCopyContext<VM>;

    // Creates a new collector
    fn new(
        vm_map: &'static VMMap,
        mmapper: &'static Mmapper,
        options: Arc<UnsafeOptionsWrapper>,
        scheduler: &'static MMTkScheduler<Self::VM>
    ) -> Self {
        // This HeapMeta structure allows us to reserve blocks of memory
        // either at the start or the end of the heap
        let mut heap = HeapMeta::new(HEAP_START, HEAP_END);
        
        MyGC {
            hi: AtomicBool::new(false),
            copyspace0: CopySpace::new( // Init as tospace
                "copyspace0",
                false,
                true,
                VMRequest::discontiguous(),
                vm_map,
                mmapper,
                &mut heap,
            ),
            copyspace1: CopySpace::new( // Init as fromspace
                "copyspace1",
                true,
                true,
                VMRequest::discontiguous(),
                vm_map,
                mmapper,
                &mut heap,
            ),
            common: CommonPlan::new(vm_map, mmapper, options, heap),
        }
    }

    // After calling new(), we can initialise the GC
    fn gc_init(
        &mut self,
        heap_size: usize, // Initialise the heap to this size
        vm_map: &'static VMMap, // ???
        scheduler: &Arc<MMTkScheduler<Self::VM>>
    ) {
        self.common.gc_init(heap_size, vm_map, scheduler);

        self.copyspace0.init(&vm_map);
        self.copyspace1.init(&vm_map);
    }

    // Helper function to access base attribute as a reference
    fn base(&self) -> &BasePlan<VM> {
        &self.common.base
    }

    fn common(&self) -> &CommonPlan<VM> {
        &self.common
    }

    fn bind_mutator(
        &'static self,
        tls: OpaquePointer, // equivalent to a C *void pointer
        _mmtk: &'static MMTK<Self::VM>, // current MMTk instance
    ) -> Box<Mutator<Self>> {
        Box::new(create_ss_mutator(tls, self))
    }

    // Flip and decide which space is the to/from spaces
    fn prepare(&self, tls: OpaquePointer) {
        // Large object/immortal spaces do their own thing - this is abstracted away
        self.common.prepare(tls, true);

        // Flip the semispaces, then prepare each of the regions for copying
        self.hi
            .store(!self.hi.load(Ordering::SeqCst), Ordering::SeqCst);
        let hi = self.hi.load(Ordering::SeqCst);
        self.copyspace0.prepare(hi); // Let each space internally remember
        self.copyspace1.prepare(!hi); // whether it is 'from' or 'to'
    }

    fn release(&self, tls: OpaquePointer) {
        self.common.release(tls, true);
        // release the collected region
        self.fromspace().release();
    }

    // We only use a bump pointer allocator in NoGC
    fn get_allocator_mapping(&self) -> &'static EnumMap<AllocationSemantics, AllocatorSelector> {
        &*ALLOCATOR_MAPPING
    }

    fn schedule_collection(&'static self, scheduler: &MMTkScheduler<VM>) {
        // Force an emergency (full?) collection if required
        self.base().set_collection_kind();
        // Status message - presumably for debugging and thread safety?
        self.base().set_gc_status(GcStatus::GcPrepare);
        // Stop & scan mutators (mutator scanning can happen before STW)
        scheduler
            .unconstrained_works
            .add(StopMutators::<SSProcessEdges<VM>>::new());
        // Prepare global/collectors/mutators
        scheduler.prepare_stage.add(Prepare::new(self));
        // Release global/collectors/mutators
        scheduler.release_stage.add(Release::new(self));
        // Resume mutators
        #[cfg(feature = "sanity")]
        scheduler.final_stage.add(ScheduleSanityGC);
        scheduler.set_finalizer(Some(EndOfGC));
    }

    // Find the number of reserved pages, but only for copying spaces
    fn get_collection_reserve(&self) -> usize {
        self.tospace().reserved_pages()
    }

    // Mumber of reserved pages, but including immortal/LOS
    fn get_pages_used(&self) -> usize {
        self.tospace().reserved_pages() + self.common.get_pages_used()
    }
}

// Helper functions which allow us to easily determine which space is the to/from space at the moment
impl<VM: VMBinding> MyGC<VM> {
    pub fn tospace(&self) -> &CopySpace<VM> {
        if self.hi.load(Ordering::SeqCst) {
            &self.copyspace1
        } else {
            &self.copyspace0
        }
    }

    pub fn fromspace(&self) -> &CopySpace<VM> {
        if self.hi.load(Ordering::SeqCst) {
            &self.copyspace0
        } else {
            &self.copyspace1
        }
    }
}