use std::sync::{Mutex, RwLock, Condvar, Arc};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::collections::LinkedList;
use std::ptr;
use crate::vm::VMBinding;
use crate::mmtk::MMTK;
use crate::util::OpaquePointer;
use super::work::{GCWork, Work};
use super::worker::{WorkerGroup, Worker};
use crate::vm::Collection;
use std::collections::BinaryHeap;
use std::cmp;
use crate::plan::Plan;
use super::*;



struct PrioritizedWork<C: Context> {
    priority: usize,
    work: Box<dyn Work<C>>,
}

impl <C: Context> PartialEq for PrioritizedWork<C> {
    fn eq(&self, other: &Self) -> bool {
        self.priority == other.priority && &self.work.as_ref() as *const _ == &other.work.as_ref() as *const _
    }
}

impl <C: Context> Eq for PrioritizedWork<C> {}

impl <C: Context> Ord for PrioritizedWork<C> {
    fn cmp(&self, other: &Self) -> cmp::Ordering {
        // other.0.cmp(&self.0)
        self.priority.cmp(&other.priority)
    }
}

impl <C: Context> PartialOrd for PrioritizedWork<C> {
    fn partial_cmp(&self, other: &Self) -> Option<cmp::Ordering> {
        Some(self.cmp(other))
    }
}

pub struct WorkBucket<C: Context> {
    active: AtomicBool,
    /// A priority queue
    queue: RwLock<BinaryHeap<PrioritizedWork<C>>>,
    monitor: Arc<(Mutex<()>, Condvar)>,
    pub active_priority: AtomicUsize,
    can_open: Option<Box<dyn Fn() -> bool>>,
}

unsafe impl <C: Context> Send for WorkBucket<C> {}
unsafe impl <C: Context> Sync for WorkBucket<C> {}

impl <C: Context> WorkBucket<C> {
    pub fn new(active: bool, monitor: Arc<(Mutex<()>, Condvar)>) -> Self {
        Self {
            active: AtomicBool::new(active),
            queue: Default::default(),
            monitor,
            active_priority: AtomicUsize::new(usize::max_value()),
            can_open: None,
        }
    }
    pub fn is_activated(&self) -> bool {
        self.active.load(Ordering::SeqCst)
    }
    pub fn active_priority(&self) -> usize {
        self.active_priority.load(Ordering::SeqCst)
    }
    /// Enable the bucket
    pub fn activate(&self) {
        self.active.store(true, Ordering::SeqCst);
    }
    /// Test if the bucket is drained
    pub fn is_empty(&self) -> bool {
        self.queue.read().unwrap().len() == 0
    }
    pub fn is_drained(&self) -> bool {
        self.is_activated() && self.is_empty()
    }
    /// Disable the bucket
    pub fn deactivate(&self) {
        debug_assert!(self.queue.read().unwrap().is_empty(), "Bucket not drained before close");
        self.active.store(false, Ordering::SeqCst);
        self.active_priority.store(usize::max_value(), Ordering::SeqCst);
    }
    /// Add a work packet to this bucket, with a given priority
    pub fn add_with_priority<W: Work<C>>(&self, priority: usize, work: W) {
        let _guard = self.monitor.0.lock().unwrap();
        self.monitor.1.notify_all();
        self.queue.write().unwrap().push(PrioritizedWork { priority, work: box work });
    }
    /// Add a work packet to this bucket, with a default priority (1000)
    pub fn add<W: Work<C>>(&self, work: W) {
        self.add_with_priority(1000, work);
    }
    /// Get a work packet (with the greatest priority) from this bucket
    pub fn poll(&self) -> Option<Box<dyn Work<C>>> {
        if !self.active.load(Ordering::SeqCst) { return None }
        self.queue.write().unwrap().pop().map(|v| v.work)
    }
    pub fn set_open_condition(&mut self, pred: impl Fn() -> bool + 'static) {
        self.can_open = Some(box pred);
    }
    pub fn update(&self) -> bool {
        if let Some(can_open) = self.can_open.as_ref() {
            if !self.is_activated() && can_open() {
                self.activate();
                return true;
            }
        }
        false
    }
}