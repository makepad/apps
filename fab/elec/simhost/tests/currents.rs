use fab_elec_simhost::{currents::{compute, CurrentsJob, CurrentsLink, SegCurrents}, Spawn};
use fab_elec_view::live::segs::Resolved;
use fab_elec_view::live::net::{NetCurrents, NetTree};
use std::{cell::Cell, sync::{mpsc::{channel, Receiver, Sender}, Arc}, thread, time::Duration};

#[test]
fn segment_currents_on_a_worker() {
    struct ThreadSpawn {
        start: Cell<Option<Receiver<()>>>,
        done: Sender<()>,
    }

    impl Spawn for ThreadSpawn {
        fn worker(&self, _name: &'static str, f: Box<dyn FnOnce() + Send>) {
            let start = self.start.take();
            let done = self.done.clone();
            let _ = thread::spawn(move || {
                if let Some(start) = start {
                    if start.recv().is_err() {
                        return;
                    }
                }
                f();
                let _ = done.send(());
            });
        }

        fn job(&self, f: Box<dyn FnOnce() + Send>) {
            self.worker("elec currents test job", f);
        }
    }

    fn kcl(tree: &NetTree, currents: &[f32], pin_i: &[f32]) {
        assert_eq!(currents.len(), tree.segs.len(), "KCL checks every segment of the tree");
        for node in 0..tree.nodes {
            let mut residual = 0.0f64;
            for &(pin_node, index) in &tree.pins {
                if pin_node == node {
                    residual += pin_i.get(index as usize).copied().unwrap_or_default() as f64;
                }
            }
            for (&(a, b), &current) in tree.segs.iter().zip(currents) {
                if a == node {
                    residual += current as f64;
                }
                if b == node {
                    residual -= current as f64;
                }
            }
            assert!(residual.abs() <= 1e-6, "KCL at node {node}: residual {residual} A exceeds 1e-6 A");
        }
    }

    let trees = Arc::new(vec![
        Resolved {
            tree: NetTree {
                nodes: 4,
                segs: vec![(0, 1), (2, 1), (2, 3)],
                pins: vec![(0, 0), (3, 1)],
                ..NetTree::default()
            },
            rows: vec![7, 8, 9],
        },
        Resolved {
            tree: NetTree {
                nodes: 4,
                segs: vec![(0, 1), (1, 2), (1, 3)],
                pins: vec![(0, 2), (2, 3), (3, 4)],
                ..NetTree::default()
            },
            rows: vec![3, 4, 5],
        },
    ]);
    let pin_i: Arc<[f32]> = Arc::from([-0.004, 0.004, -0.005, 0.002, 0.003]);
    let job = CurrentsJob { seq: 3, trees: trees.clone(), pin_i: pin_i.clone() };
    let mut nc = NetCurrents::new();
    let mut scratch = vec![42.0];
    let mut out = SegCurrents { seq: 99, rows: vec![99], i: vec![42.0], unconverged: 99 };
    compute(&mut nc, &job, &mut scratch, &mut out);
    assert_eq!(out.seq, 3, "inline computation carries the job sequence");
    assert_eq!(out.rows, [7, 8, 9, 3, 4, 5], "inline computation clears old rows and preserves tree and segment order");
    assert_eq!(out.i.len(), out.rows.len(), "every output row has exactly one current");
    assert_eq!(out.unconverged, nc.unconverged(), "inline computation reports the scratch's unconverged count");
    assert_eq!(out.unconverged, 0, "acyclic trees need no unconverged loop solves");
    for (resolved, currents) in trees.iter().zip(out.i.chunks_exact(3)) {
        kcl(&resolved.tree, currents, &pin_i);
    }

    let expected = out.clone();
    let buffers = (out.rows.as_ptr(), out.i.as_ptr(), scratch.as_ptr());
    compute(&mut nc, &job, &mut scratch, &mut out);
    assert_eq!(out, expected, "repeated computation replaces output with the same rows and values");
    assert_eq!((out.rows.as_ptr(), out.i.as_ptr(), scratch.as_ptr()), buffers, "repeated computation reuses output and scratch storage");

    let mut short = trees[0].clone();
    short.rows.truncate(2);
    let short_job = CurrentsJob { seq: 4, trees: Arc::new(vec![short]), pin_i: pin_i.clone() };
    compute(&mut nc, &short_job, &mut scratch, &mut out);
    assert_eq!(out.rows, [7, 8], "short row lists contribute only their common prefix with segments");
    assert_eq!(out.i, expected.i[..2], "short row lists retain the matching currents without stale values");
    assert_eq!(out.seq, 4, "short row lists still publish their job sequence");

    let (start, ready) = channel();
    let (done, finished) = channel();
    let spawn = ThreadSpawn { start: Cell::new(Some(ready)), done };
    let mut link = CurrentsLink::new(&spawn);
    assert!(link.take().is_none(), "a new worker has no publication to take");
    assert!(!link.backlogged(), "a new link has no kept job");
    assert_eq!(link.submit(trees.clone(), Arc::from([0.0; 5])), 1, "the first submission has sequence one");
    assert_eq!(link.submit(trees.clone(), Arc::from([-0.002, 0.002, -0.003, 0.001, 0.002])), 2, "the second submission has sequence two");
    assert_eq!(link.submit(trees, pin_i), 3, "the newest submission has sequence three");
    assert!(link.backlogged(), "a third job is retained when both channel slots are occupied");
    assert!(link.waiting(), "a kept or sent job without its result waits: the host asks a frame for it");
    link.pump();
    assert!(link.backlogged(), "pumping a full channel retains the newest job");
    assert!(start.send(()).is_ok(), "the worker can start after the bounded channel fills");

    let result = loop {
        link.pump();
        if let Some(result) = link.take() {
            if result.seq == 3 {
                break result;
            }
        }
        thread::sleep(Duration::from_millis(1));
    };
    assert_eq!(result.seq, 3, "the worker eventually computes the newest queued job");
    assert_eq!(result.as_ref(), &expected, "worker currents and rows equal inline computation for the newest job");
    assert!(link.take().is_none(), "taking a publication twice yields no second result");
    assert!(!link.backlogged(), "pumping eventually sends the kept job");
    assert!(!link.waiting(), "with the newest job's result taken nothing waits, and nothing woke the host for it");

    drop(link);
    assert_eq!(finished.recv(), Ok(()), "dropping the link lets the worker closure return");
}
