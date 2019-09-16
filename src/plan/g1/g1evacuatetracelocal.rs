use plan::{TraceLocal, TransitiveClosure};
use plan::g1::PLAN;
use plan::trace::Trace;
use policy::space::Space;
use util::{Address, ObjectReference};
use util::queue::LocalQueue;
use vm::*;
use libc::c_void;
use super::g1;
use policy::region::*;
use ::util::heap::layout::Mmapper;
use ::util::heap::layout::heap_layout::MMAPPER;

pub struct G1EvacuateTraceLocal {
    tls: *mut c_void,
    values: LocalQueue<'static, ObjectReference>,
    root_locations: LocalQueue<'static, Address>,
}

impl TransitiveClosure for G1EvacuateTraceLocal {
    fn process_edge(&mut self, src: ObjectReference, slot: Address) {
        debug_assert!(MMAPPER.address_is_mapped(slot));
        let object: ObjectReference = unsafe { slot.load() };
        let new_object = self.trace_object(object);
        if self.overwrite_reference_during_trace() {
            if super::ENABLE_REMEMBERED_SETS {
                // src.slot -> new_object
                if RegionSpace::is_cross_region_ref(src, slot, new_object) && PLAN.region_space.in_space(new_object) {
                    Region::of_object(new_object).remset().add_card(Card::of(src))
                }

                // if !new_object.is_null() && PLAN.region_space.in_space(new_object) {
                //     let other_region = Region::of_object(new_object);
                //     debug_assert!(other_region.committed);
                //     if other_region != Region::of_object(src) {
                //         other_region.remset().add_card(Card::of(src))
                //     }
                // }
            }
            unsafe { slot.store(new_object) };
        }
    }

    fn process_node(&mut self, object: ObjectReference) {
        self.values.enqueue(object);
    }
}

impl TraceLocal for G1EvacuateTraceLocal {
    fn process_remembered_sets(&mut self) {
    }

    fn overwrite_reference_during_trace(&self) -> bool {
        true
    }

    fn process_roots(&mut self) {
        while let Some(slot) = self.root_locations.dequeue() {
            self.process_root_edge(slot, true)
        }
        debug_assert!(self.root_locations.is_empty());
    }

    fn process_root_edge(&mut self, slot: Address, _untraced: bool) {
        let object: ObjectReference = unsafe { slot.load() };
        let new_object = self.trace_object(object);
        if self.overwrite_reference_during_trace() {
            unsafe { slot.store(new_object) };
        }
    }

    fn trace_object(&mut self, object: ObjectReference) -> ObjectReference {
        let tls = self.tls;

        if super::ENABLE_REMEMBERED_SETS {
            if object.is_null() {
                object
            } else if PLAN.region_space.in_space(object) {
                let region = Region::of_object(object);
                debug_assert!(region.committed);
                if region.relocate {
                    if region.prev_mark_table().is_marked(object) {
                        let allocator = Self::pick_copy_allocator(object);
                        PLAN.region_space.trace_evacuate_object_in_cset(self, object, allocator, tls)
                    } else {
                        ObjectReference::null()
                    }
                } else {
                    object
                }
            } else {
                debug_assert!(PLAN.is_mapped_object(object));
                object
            }
        } else {
            if object.is_null() {
                object
            } else if PLAN.region_space.in_space(object) {
                debug_assert!(Region::of_object(object).committed);
                let allocator = Self::pick_copy_allocator(object);
                PLAN.region_space.trace_evacuate_object(self, object, allocator, tls)
            } else if PLAN.versatile_space.in_space(object) {
                PLAN.versatile_space.trace_object(self, object)
            } else if PLAN.los.in_space(object) {
                PLAN.los.trace_object(self, object)
            } else if PLAN.vm_space.in_space(object) {
                PLAN.vm_space.trace_object(self, object)
            } else {
                unreachable!("{:?}", object)
            }
        }
    }

    fn complete_trace(&mut self) {
        let id = self.tls;
        self.process_roots();
        debug_assert!(self.root_locations.is_empty());
        loop {
            while let Some(object) = self.values.dequeue() {
                VMScanning::scan_object(self, object, id);
            }
            self.process_remembered_sets();
            if self.values.is_empty() {
                break;
            }
        }
        debug_assert!(self.root_locations.is_empty());
        debug_assert!(self.values.is_empty());
    }

    fn release(&mut self) {
        self.root_locations.reset();
        self.values.reset();
    }

    fn process_interior_edge(&mut self, target: ObjectReference, slot: Address, _root: bool) {
        let interior_ref: Address = unsafe { slot.load() };
        let offset = interior_ref - target.to_address();
        let new_target = self.trace_object(target);
        if self.overwrite_reference_during_trace() {
            unsafe { slot.store(new_target.to_address() + offset) };
        }
    }

    fn report_delayed_root_edge(&mut self, slot: Address) {
        self.root_locations.enqueue(slot);
    }

    fn will_not_move_in_current_collection(&self, obj: ObjectReference) -> bool {
        if PLAN.region_space.in_space(obj) {
            false
        } else {
            true
        }
    }

    fn is_live(&self, object: ObjectReference) -> bool {
        if super::ENABLE_REMEMBERED_SETS {
            if object.is_null() {
                return false;
            } else if PLAN.region_space.in_space(object) {
                if Region::of_object(object).relocate {
                    ::util::forwarding_word::is_forwarded_or_being_forwarded(object)
                } else {
                    true
                }
            } else {
                debug_assert!(PLAN.is_mapped_object(object));
                true
            }
        } else {
            if object.is_null() {
                return false;
            } else if PLAN.region_space.in_space(object) {
                debug_assert!(Region::of_object(object).committed);
                if Region::of_object(object).relocate {
                    if ::util::forwarding_word::is_forwarded_or_being_forwarded(object) {
                        return true;
                    } else {
                        return false
                    }
                } else {
                    PLAN.region_space.is_live_next(object)
                }
            } else if PLAN.versatile_space.in_space(object) {
                true
            } else if PLAN.los.in_space(object) {
                PLAN.los.is_live(object)
            } else if PLAN.vm_space.in_space(object) {
                true
            } else {
                unreachable!()
            }
        }
    }
}

impl G1EvacuateTraceLocal {
    pub fn new(trace: &'static Trace) -> Self {
        Self {
            tls: 0 as *mut c_void,
            values: trace.values.spawn_local(),
            root_locations: trace.root_locations.spawn_local(),
        }
    }

    pub fn init(&mut self, tls: *mut c_void) {
        self.tls = tls;
    }

    pub fn is_empty(&self) -> bool {
        self.root_locations.is_empty() && self.values.is_empty()
    }
    
    pub fn flush(&mut self) {
        self.values.flush();
        self.root_locations.flush();
    }

    #[inline]
    fn pick_copy_allocator(o: ObjectReference) -> ::plan::Allocator {
        if !super::ENABLE_GENERATIONAL_GC {
            return g1::ALLOC_OLD;
        }
        match Region::of_object(o).generation {
            Gen::Eden => g1::ALLOC_SURVIVOR,
            _ => g1::ALLOC_OLD,
        }
    }
}
