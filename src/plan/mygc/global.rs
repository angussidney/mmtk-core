use crate::mmtk::MMTK;
use crate::plan::global::{BasePlan, NoCopy};
use crate::plan::mutator_context::Mutator;
use crate::plan::mygc::mutator::create_nogc_mutator;
use crate::plan::mygc::mutator::ALLOCATOR_MAPPING; // TODO change these
use crate::plan::AllocationSemantics;
use crate::plan::Plan;
use crate::policy::space::Space;
use crate::scheduler::MMTkScheduler;
use crate::util::alloc::allocators::AllocatorSelector;
use crate::util::heap::layout::heap_layout::Mmapper;
use crate::util::heap::layout::heap_layout::VMMap;
use crate::util::heap::layout::vm_layout_constants::{HEAP_END, HEAP_START};
use crate::util::heap::HeapMeta;
#[allow(unused_imports)]
use crate::util::heap::VMRequest;
use crate::util::options::UnsafeOptionsWrapper;
use crate::util::OpaquePointer;
use crate::vm::VMBinding;
use enum_map::EnumMap;
use std::sync::Arc;

// This type of immortal space has locks in place to prevent issues when
// running multithreaded code. LockFreeImmortalSpace should only be used
// for experiemental purposes
use crate::policy::immortalspace::ImmortalSpace;

pub type SelectedPlan<VM> = MyGC<VM>;

// The base struct which represents the properties of our GC.
// Currently, it is  made up of a single ImmortalSpace (i.e. a space which
// is never garbage collected)
// Also includes basic properties that are common to **all** GCs via BasePlan
pub struct MyGC<VM: VMBinding> {
    pub base: BasePlan<VM>,
    pub nogc_space: ImmortalSpace<VM>,
}

unsafe impl<VM: VMBinding> Sync for MyGC<VM> {}

// Implement the Plan trait - this represents how this plan should be
// initialised, prepared, manually collected, statistically analyised etc
impl<VM: VMBinding> Plan for MyGC<VM> {
    type VM = VM;
    type Mutator = Mutator<Self>; // Mutator: per-thread data structure that manages allocations etc
    // The default CopyContext represents a copying GC; however, we are
    // doing nothing so we use the NoCopy context
    type CopyContext = NoCopy<VM>;

    // Creates a new NoGC collector
    fn new(
        vm_map: &'static VMMap, // Something something free list allocator???
        mmapper: &'static Mmapper,
        options: Arc<UnsafeOptionsWrapper>,
        scheduler: &'static MMTkScheduler<Self::VM>
    ) -> Self {
        // This HeapMeta structure allows us to reserve blocks of memory
        // either at the start or the end of the heap
        let mut heap = HeapMeta::new(HEAP_START, HEAP_END);
        // Init the immortal space to be used for our NoGC algorithm
        let nogc_space = ImmortalSpace::new(
            "mygc_space", // name
            true, // zeroed
            VMRequest::discontiguous(), // doesn't have to be next to prexisting data
            vm_map, // I think this
            mmapper, // (and this) has something to do with virtual->real mappings
            &mut heap,
        );

        MyGC {
            nogc_space,
            base: BasePlan::new(vm_map, mmapper, options, heap),
        }
    }

    // After calling new(), we can initialise the GC
    fn gc_init(
        &mut self,
        heap_size: usize, // Initialise the heap to this size
        vm_map: &'static VMMap, // ???
        scheduler: &Arc<MMTkScheduler<Self::VM>>
    ) {
        // Do basic GC initialisation common to all GCs
        self.base.gc_init(heap_size, vm_map, scheduler);

        // Immortal space init
        self.nogc_space.init(&vm_map);
    }

    // Helper function to access base attribute as a reference
    fn base(&self) -> &BasePlan<VM> {
        &self.base
    }

    fn bind_mutator(
        &'static self,
        tls: OpaquePointer, // equivalent to a C *void pointer
        _mmtk: &'static MMTK<Self::VM>, // current MMTk instance
    ) -> Box<Mutator<Self>> {
        Box::new(create_nogc_mutator(tls, self))
    }

    fn prepare(&self, _tls: OpaquePointer) {
        unreachable!()
    }

    fn release(&self, _tls: OpaquePointer) {
        unreachable!()
    }

    // We only use a bump pointer allocator in NoGC
    fn get_allocator_mapping(&self) -> &'static EnumMap<AllocationSemantics, AllocatorSelector> {
        &*ALLOCATOR_MAPPING
    }

    // You can't GC a NoGC...
    fn schedule_collection(&'static self, _scheduler: &MMTkScheduler<VM>) {
        unreachable!("GC triggered in nogc")
    }

    fn get_pages_used(&self) -> usize {
        self.nogc_space.reserved_pages()
    }

    // You can't GC a NoGC...
    fn handle_user_collection_request(&self, _tls: OpaquePointer, _force: bool) {
        println!("Warning: User attempted a collection request, but it is not supported in NoGC. The request is ignored.");
    }
}