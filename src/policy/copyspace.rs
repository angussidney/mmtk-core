use crate::plan::{Allocator, CopyContext};
use crate::plan::TransitiveClosure;
use crate::policy::space::{CommonSpace, Space, SFT};
use crate::util::constants::CARD_META_PAGES_PER_REGION;
use crate::util::forwarding_word as ForwardingWord;
use crate::util::heap::VMRequest;
use crate::util::heap::{MonotonePageResource, PageResource};
use crate::util::{Address, ObjectReference};

use crate::policy::space::SpaceOptions;
use crate::util::heap::layout::heap_layout::{Mmapper, VMMap};
use crate::util::heap::HeapMeta;
use crate::vm::*;
use std::sync::atomic::{AtomicBool, Ordering};
//use crate::mmtk::SFT_MAP;
use libc::{mprotect, PROT_EXEC, PROT_NONE, PROT_READ, PROT_WRITE};
use std::cell::UnsafeCell;
use crate::util::metadata::*;
use std::sync::Mutex;
use crate::util::conversions;
use crate::mmtk::SFT_MAP;
#[cfg(feature = "gencopy_sanity_gc")]
use crate::util::constants::*;
#[cfg(feature = "gencopy_sanity_gc")]
use crate::util::heap::layout::Mmapper as MmapperTrait;
#[cfg(feature = "gencopy_sanity_gc")]
use crate::util::heap::layout::vm_layout_constants::*;

unsafe impl<VM: VMBinding> Sync for CopySpace<VM> {}

#[cfg(feature = "gencopy_sanity_gc")]
const fn max(a: usize, b: usize) -> usize {
    [a, b][(a < b) as usize]
}

#[cfg(not(feature = "gencopy_sanity_gc"))]
const META_DATA_PAGES_PER_REGION: usize = CARD_META_PAGES_PER_REGION;
#[cfg(feature = "gencopy_sanity_gc")]
const META_DATA_PAGES_PER_REGION: usize = max(CARD_META_PAGES_PER_REGION, <MarkBitMap as PerChunkMetadata>::METADATA_PAGES_PER_CHUNK);

pub struct CopySpace<VM: VMBinding> {
    common: UnsafeCell<CommonSpace<VM>>,
    pr: MonotonePageResource<VM>,
    from_space: AtomicBool,
    mark_tables: Mutex<Vec<&'static MarkBitMap>>,
}

impl<VM: VMBinding> SFT for CopySpace<VM> {
    fn is_live(&self, object: ObjectReference) -> bool {
        !self.from_space() || ForwardingWord::is_forwarded::<VM>(object)
    }
    fn is_movable(&self) -> bool {
        true
    }
    #[cfg(feature = "sanity")]
    fn is_sane(&self) -> bool {
        !self.from_space()
    }
    fn initialize_header(&self, _object: ObjectReference, _alloc: bool) {}
}

impl<VM: VMBinding> Space<VM> for CopySpace<VM> {
    fn as_space(&self) -> &dyn Space<VM> {
        self
    }
    fn as_sft(&self) -> &(dyn SFT + Sync + 'static) {
        self
    }
    fn get_page_resource(&self) -> &dyn PageResource<VM> {
        &self.pr
    }
    fn common(&self) -> &CommonSpace<VM> {
        unsafe { &*self.common.get() }
    }
    unsafe fn unsafe_common_mut(&self) -> &mut CommonSpace<VM> {
        &mut *self.common.get()
    }

    fn grow_space(&self, start: Address, bytes: usize, new_chunk: bool) {
        if new_chunk {
            let chunks = conversions::bytes_to_chunks_up(bytes);
            SFT_MAP.update(self.as_sft() as *const (dyn SFT + Sync), start, chunks);
        }
        #[cfg(feature = "gencopy_sanity_gc")] {
            let _chunk = conversions::chunk_align_down(start);
            self.common().mmapper.ensure_mapped(start, bytes >> LOG_BYTES_IN_PAGE);
            let mut mark_tables = self.mark_tables.lock().unwrap();
            let mut mark_table;
            for offset in (0..bytes).step_by(BYTES_IN_CHUNK) {
                mark_table = MarkBitMap::of(start + offset);
                if !mark_tables.contains(&mark_table) {
                    mark_table.clear();
                    mark_tables.push(mark_table);
                }
            }
            mark_table = MarkBitMap::of(start + bytes - 1);
            if !mark_tables.contains(&mark_table) {
                mark_table.clear();
                mark_tables.push(mark_table);
            }
        }
    }

    fn init(&mut self, _vm_map: &'static VMMap) {
        // Borrow-checker fighting so that we can have a cyclic reference
        let me = unsafe { &*(self as *const Self) };
        self.pr.bind_space(me);
    }

    fn release_multiple_pages(&mut self, _start: Address) {
        panic!("copyspace only releases pages enmasse")
    }
}

impl<VM: VMBinding> CopySpace<VM> {
    pub fn new(
        name: &'static str,
        from_space: bool,
        zeroed: bool,
        vmrequest: VMRequest,
        vm_map: &'static VMMap,
        mmapper: &'static Mmapper,
        heap: &mut HeapMeta,
    ) -> Self {
        let common = CommonSpace::new(
            SpaceOptions {
                name,
                movable: true,
                immortal: false,
                zeroed,
                vmrequest,
            },
            vm_map,
            mmapper,
            heap,
        );
        CopySpace {
            pr: if vmrequest.is_discontiguous() {
                MonotonePageResource::new_discontiguous(META_DATA_PAGES_PER_REGION, vm_map)
            } else {
                MonotonePageResource::new_contiguous(
                    common.start,
                    common.extent,
                    META_DATA_PAGES_PER_REGION,
                    vm_map,
                )
            },
            common: UnsafeCell::new(common),
            from_space: AtomicBool::new(from_space),
            mark_tables: Default::default(),
        }
    }

    pub fn prepare(&self, from_space: bool) {
        self.from_space.store(from_space, Ordering::SeqCst);
        #[cfg(feature = "gencopy_sanity_gc")] {
            let mark_tables = self.mark_tables.lock().unwrap();
            for mark_table in mark_tables.iter() {
                mark_table.clear();
            }
        }
    }

    pub fn release(&self) {
        #[cfg(feature = "gencopy_sanity_gc")]
        self.mark_tables.lock().unwrap().clear();
        unsafe { self.pr.reset(); }
        self.from_space.store(false, Ordering::SeqCst);
    }

    pub fn sanity_prepare(&self) {
        #[cfg(feature = "gencopy_sanity_gc")] {
            let mark_tables = self.mark_tables.lock().unwrap();
            for mark_table in mark_tables.iter() {
                mark_table.clear();
            }
        }
    }

    pub fn sanity_release(&self) {
        #[cfg(feature = "gencopy_sanity_gc")]
        self.mark_tables.lock().unwrap().clear();
    }

    fn from_space(&self) -> bool {
        self.from_space.load(Ordering::SeqCst)
    }

    pub fn trace_mark_object(&self, trace: &mut impl TransitiveClosure, object: ObjectReference) -> ObjectReference {
        let addr = VM::VMObjectModel::object_start_ref(object);
        let mark_bitmap = MarkBitMap::of(addr);
        {
            let mark_tables = self.mark_tables.lock().unwrap();
            debug_assert!(mark_tables.contains(&mark_bitmap), "Invalid chunk {:?}", crate::util::conversions::chunk_align_down(addr) );
        }
        if mark_bitmap.attempt_mark(addr) {
            trace.process_node(object);
        }
        object
    }

    pub fn trace_object<T: TransitiveClosure>(
        &self,
        trace: &mut T,
        object: ObjectReference,
        allocator: Allocator,
        copy_context: &mut impl CopyContext,
    ) -> ObjectReference {
        trace!(
            "copyspace.trace_object(, {:?}, {:?})",
            object,
            allocator,
        );
        if !self.from_space() {
            return object;
        }
        trace!("attempting to forward");
        let forwarding_status = ForwardingWord::attempt_to_forward::<VM>(object);
        trace!("checking if object is being forwarded");
        if ForwardingWord::state_is_forwarded_or_being_forwarded(forwarding_status) {
            trace!("... yes it is");
            let new_object =
                ForwardingWord::spin_and_get_forwarded_object::<VM>(object, forwarding_status);
                trace!("Returning");
            new_object
        } else {
            trace!("... no it isn't. Copying");
            let new_object = ForwardingWord::forward_object::<VM, _>(object, allocator, copy_context);
            trace!("Forwarding pointer");
            trace.process_node(new_object);
            trace!("Copying [{:?} -> {:?}]", object, new_object);
            new_object
        }
    }

    pub fn protect(&self) {
        if !self.common().contiguous {
            panic!(
                "Implement Options.protectOnRelease for MonotonePageResource.release_pages_extent"
            )
        }
        let start = self.common().start;
        let extent = self.common().extent;
        unsafe {
            mprotect(start.to_mut_ptr(), extent, PROT_NONE);
        }
        trace!("Protect {:x} {:x}", start, start + extent);
    }

    pub fn unprotect(&self) {
        if !self.common().contiguous {
            panic!(
                "Implement Options.protectOnRelease for MonotonePageResource.release_pages_extent"
            )
        }
        let start = self.common().start;
        let extent = self.common().extent;
        unsafe {
            mprotect(
                start.to_mut_ptr(),
                extent,
                PROT_READ | PROT_WRITE | PROT_EXEC,
            );
        }
        trace!("Unprotect {:x} {:x}", start, start + extent);
    }
}
