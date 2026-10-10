//! Bounded independent work, with one active job per shared service group.
use std::collections::{HashSet, VecDeque};
use std::future::Future;
use tokio::task::JoinSet;

pub async fn run<T, R, F, Fut>(jobs: Vec<(String, T)>, concurrency: usize, execute: F) -> Vec<R>
where
    T: Send + 'static,
    R: Send + 'static,
    F: Fn(T) -> Fut + Clone,
    Fut: Future<Output = R> + Send + 'static,
{
    assert!(concurrency > 0, "concurrency must be positive");
    let mut results: Vec<Option<R>> = (0..jobs.len()).map(|_| None).collect();
    let mut pending: VecDeque<_> = jobs.into_iter().enumerate().collect();
    let mut active = HashSet::new();
    let mut running = JoinSet::new();
    loop {
        while running.len() < concurrency {
            // A queued model sharing an active endpoint must not block other endpoints.
            let Some(next) = pending
                .iter()
                .position(|(_, (group, _))| !active.contains(group))
            else {
                break;
            };
            let (index, (group, job)) = pending.remove(next).unwrap();
            active.insert(group.clone());
            let future = execute.clone()(job);
            running.spawn(async move { (index, group, future.await) });
        }
        let Some(joined) = running.join_next().await else {
            break;
        };
        let (index, group, result) = joined.expect("benchmark worker panicked");
        active.remove(&group);
        results[index] = Some(result);
    }
    results
        .into_iter()
        .map(|r| r.expect("scheduled job was not completed"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    #[tokio::test]
    async fn independent_groups_overlap_shared_groups_do_not_and_results_keep_input_order() {
        let state = Arc::new(Mutex::new((HashSet::new(), 0, Vec::new())));
        let jobs = vec![
            ("a".into(), (0, "a")),
            ("a".into(), (1, "a")),
            ("b".into(), (2, "b")),
            ("c".into(), (3, "c")),
        ];
        let observed = state.clone();
        let results = run(jobs, 2, move |(index, group)| {
            let state = observed.clone();
            async move {
                {
                    let mut s = state.lock().unwrap();
                    assert!(s.0.insert(group), "same group overlapped");
                    s.1 = s.1.max(s.0.len());
                    s.2.push(index);
                }
                tokio::time::sleep(Duration::from_millis(if index == 0 { 30 } else { 2 })).await;
                state.lock().unwrap().0.remove(group);
                index
            }
        })
        .await;
        assert_eq!(results, vec![0, 1, 2, 3]);
        let state = state.lock().unwrap();
        assert_eq!(state.1, 2);
        assert_eq!(&state.2[..2], &[0, 2]);
    }

    #[tokio::test]
    async fn sequential_mode_and_failed_jobs_preserve_every_slot() {
        let results = run(vec![("a".into(), 1), ("b".into(), 2)], 1, |n| async move {
            if n == 1 {
                Err("transport failed")
            } else {
                Ok(n)
            }
        })
        .await;
        assert_eq!(results, vec![Err("transport failed"), Ok(2)]);
        let empty: Vec<usize> = run(Vec::<(String, usize)>::new(), 1, |n| async move { n }).await;
        assert!(empty.is_empty());
    }
}
