//! Bounded worker concurrency harness with backpressure.
//!
//! Tests that worker pools respect concurrency limits and apply backpressure
//! to prevent resource exhaustion during concurrent mutations.

use alloc::vec::Vec;

/// WorkerState tracks the execution and resource state of a concurrent worker.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WorkerState {
    Idle,
    Active,
    Backpressured,
    Failed,
}

/// WorkerLoad represents the resource demand of a pending work item.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkerLoad {
    pub work_id: u64,
    pub resource_cost: u32,
    pub priority: u32,
}

/// BoundedConcurrencyPool manages worker concurrency with backpressure.
#[derive(Clone, Debug)]
pub struct BoundedConcurrencyPool {
    max_workers: u32,
    max_resource_budget: u32,
    current_workers: u32,
    current_resource_usage: u32,
    work_queue: Vec<WorkerLoad>,
    completed_work: Vec<u64>,
    rejected_work: Vec<u64>,
}

impl BoundedConcurrencyPool {
    /// Create a new bounded concurrency pool.
    pub fn new(max_workers: u32, max_resource_budget: u32) -> Self {
        Self {
            max_workers,
            max_resource_budget,
            current_workers: 0,
            current_resource_usage: 0,
            work_queue: Vec::new(),
            completed_work: Vec::new(),
            rejected_work: Vec::new(),
        }
    }

    /// Submit work to the pool, applying backpressure if limits are exceeded.
    pub fn submit_work(&mut self, work: WorkerLoad) -> Result<(), WorkerLoad> {
        if self.can_accept_work(&work) {
            self.work_queue.push(work);
            Ok(())
        } else {
            self.rejected_work.push(work.work_id);
            Err(work)
        }
    }

    /// Check if the pool can accept more work.
    fn can_accept_work(&self, work: &WorkerLoad) -> bool {
        self.current_workers < self.max_workers
            && self.current_resource_usage + work.resource_cost <= self.max_resource_budget
    }

    /// Acquire a worker slot and resource budget.
    pub fn acquire_worker(&mut self, work_id: u64, cost: u32) -> Result<(), &'static str> {
        if self.current_workers >= self.max_workers {
            return Err("Worker limit reached");
        }

        if self.current_resource_usage + cost > self.max_resource_budget {
            return Err("Resource budget exceeded");
        }

        self.current_workers = self.current_workers.saturating_add(1);
        self.current_resource_usage = self.current_resource_usage.saturating_add(cost);

        Ok(())
    }

    /// Release a worker slot and free its resources.
    pub fn release_worker(&mut self, cost: u32, success: bool) {
        self.current_workers = self.current_workers.saturating_sub(1);
        self.current_resource_usage = self.current_resource_usage.saturating_sub(cost);

        if !self.work_queue.is_empty() {
            if let Some(work) = self.work_queue.remove(0) {
                if success {
                    self.completed_work.push(work.work_id);
                } else {
                    self.rejected_work.push(work.work_id);
                }
            }
        }
    }

    /// Process work from the queue with concurrency limits.
    pub fn process_queue<F>(&mut self, mut process_fn: F) -> Result<(), &'static str>
    where
        F: FnMut(&WorkerLoad) -> Result<(), &'static str>,
    {
        let mut queue_copy = self.work_queue.clone();
        queue_copy.sort_by_key(|w| core::cmp::Reverse(w.priority));

        for work in queue_copy.iter() {
            if !self.can_accept_work(work) {
                continue;
            }

            self.acquire_worker(work.work_id, work.resource_cost)?;

            match process_fn(work) {
                Ok(()) => {
                    self.completed_work.push(work.work_id);
                    self.release_worker(work.resource_cost, true);
                }
                Err(e) => {
                    self.rejected_work.push(work.work_id);
                    self.release_worker(work.resource_cost, false);
                    return Err(e);
                }
            }
        }

        Ok(())
    }

    /// Get current worker state.
    pub fn worker_state(&self) -> WorkerState {
        if self.current_workers == 0 {
            WorkerState::Idle
        } else if self.current_workers >= self.max_workers {
            WorkerState::Backpressured
        } else {
            WorkerState::Active
        }
    }

    /// Get pending work queue size.
    pub fn pending_work(&self) -> usize {
        self.work_queue.len()
    }

    /// Get completed work count.
    pub fn completed_work_count(&self) -> usize {
        self.completed_work.len()
    }

    /// Get rejected work count (backpressure).
    pub fn rejected_work_count(&self) -> usize {
        self.rejected_work.len()
    }

    /// Verify backpressure is applied correctly.
    pub fn verify_backpressure(&self) -> Result<(), &'static str> {
        if self.current_workers > self.max_workers {
            return Err("Worker count exceeded limit");
        }

        if self.current_resource_usage > self.max_resource_budget {
            return Err("Resource usage exceeded limit");
        }

        Ok(())
    }

    /// Get current resource usage percentage.
    pub fn resource_utilization_percent(&self) -> u32 {
        if self.max_resource_budget == 0 {
            return 0;
        }
        (self.current_resource_usage * 100) / self.max_resource_budget
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;

    #[test]
    fn test_bounded_pool_creation() {
        let pool = BoundedConcurrencyPool::new(5, 1000);
        assert_eq!(pool.max_workers, 5);
        assert_eq!(pool.max_resource_budget, 1000);
    }

    #[test]
    fn test_submit_work_within_limits() {
        let mut pool = BoundedConcurrencyPool::new(5, 1000);
        let work = WorkerLoad {
            work_id: 1,
            resource_cost: 100,
            priority: 1,
        };

        let result = pool.submit_work(work);
        assert!(result.is_ok());
        assert_eq!(pool.pending_work(), 1);
    }

    #[test]
    fn test_backpressure_on_resource_limit() {
        let mut pool = BoundedConcurrencyPool::new(5, 100);
        let work1 = WorkerLoad {
            work_id: 1,
            resource_cost: 80,
            priority: 1,
        };
        let work2 = WorkerLoad {
            work_id: 2,
            resource_cost: 50,
            priority: 1,
        };

        assert!(pool.submit_work(work1).is_ok());
        assert!(pool.submit_work(work2).is_err());
    }

    #[test]
    fn test_acquire_and_release_worker() {
        let mut pool = BoundedConcurrencyPool::new(5, 1000);

        assert!(pool.acquire_worker(1, 100).is_ok());
        assert_eq!(pool.current_workers, 1);
        assert_eq!(pool.current_resource_usage, 100);

        pool.release_worker(100, true);
        assert_eq!(pool.current_workers, 0);
        assert_eq!(pool.current_resource_usage, 0);
    }

    #[test]
    fn test_worker_limit_enforcement() {
        let mut pool = BoundedConcurrencyPool::new(2, 10000);

        assert!(pool.acquire_worker(1, 100).is_ok());
        assert!(pool.acquire_worker(2, 100).is_ok());
        assert!(pool.acquire_worker(3, 100).is_err());
    }

    #[test]
    fn test_backpressure_verification() {
        let pool = BoundedConcurrencyPool::new(5, 1000);
        assert!(pool.verify_backpressure().is_ok());
    }

    #[test]
    fn test_worker_state_transitions() {
        let mut pool = BoundedConcurrencyPool::new(2, 1000);

        assert_eq!(pool.worker_state(), WorkerState::Idle);

        pool.acquire_worker(1, 100).ok();
        assert_eq!(pool.worker_state(), WorkerState::Active);

        pool.acquire_worker(2, 100).ok();
        assert_eq!(pool.worker_state(), WorkerState::Backpressured);
    }

    #[test]
    fn test_resource_utilization_percent() {
        let mut pool = BoundedConcurrencyPool::new(5, 1000);
        pool.acquire_worker(1, 500).ok();

        assert_eq!(pool.resource_utilization_percent(), 50);
    }
}
