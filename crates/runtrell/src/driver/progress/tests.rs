// SPDX-License-Identifier: Apache-2.0
use super::*;

#[derive(Clone, Copy)]
enum Step {
    View(u64),
    Write(&'static str, u64),
    Read(&'static str, u64, u64),
    DiscardMedia,
}

use Step::*;

fn replay_recovery(steps: &[Step]) {
    for compact in [false, true] {
        replay(steps, compact);
    }
}

fn replay(steps: &[Step], compact: bool) {
    let mut progress = WorkProgress::default();
    let namespace = data::Namespace {
        owner: RunHandle {
            tenant: "fixture".into(),
            id: "run".into(),
        },
        agent: "main".into(),
    };
    for (index, step) in steps.iter().enumerate() {
        let observation = match *step {
            DiscardMedia => {
                if compact {
                    progress.compacted();
                } else {
                    progress.context_changed();
                }
                // Recovery state must survive a worker restart.
                progress = serde_json::from_value(serde_json::to_value(progress).unwrap()).unwrap();
                continue;
            }
            View(page) => ToolProgress::Read {
                namespace: namespace.clone(),
                path: "document:source".into(),
                revision: 0,
                start: page,
                end: page + 1,
            },
            Write(path, revision) => ToolProgress::Write {
                namespace: namespace.clone(),
                path: path.into(),
                revision: Some(revision),
                hash: Some(format!("content-{revision}")),
            },
            Read(path, revision, bytes) => ToolProgress::Read {
                namespace: namespace.clone(),
                path: path.into(),
                revision,
                start: 0,
                end: bytes,
            },
        };
        assert!(
            !progress
                .apply(
                    &namespace.owner,
                    vec![observation],
                    NonZeroU64::new(3),
                    &format!("step-{index}")
                )
                .unwrap(),
            "stopped at step {index}"
        );
    }
    let Write(path, revision) = steps.last().unwrap() else {
        panic!("expected the next write after recovery")
    };
    assert!(matches!(
        progress.writes.get(&digest(&json!([namespace, path]))),
        Some(ToolProgress::Write { revision: Some(saved), .. }) if saved == revision
    ));
}

// Anonymized traces keep the fixed page ranges, revision identities and read
// sizes. The final write represents the next tool call after recovery.

#[test]
fn exercise_recovery_continues_writing() {
    replay_recovery(&[
        Read("part-0.md", 1, 136),
        View(1),
        Write("part-1.md", 2),
        View(2),
        Read("part-1.md", 2, 2_668),
        Write("part-1.md", 3),
        View(3),
        Write("part-2.md", 4),
        View(4),
        Write("part-3.md", 5),
        View(5),
        DiscardMedia,
        Read("part-1.md", 3, 5_282),
        Read("part-2.md", 4, 2_604),
        Read("part-3.md", 5, 2_600),
        Write("part-4.md", 6),
        View(6),
        Write("part-5.md", 7),
        View(7),
        Write("part-6.md", 8),
        View(8),
        Write("part-7.md", 9),
        View(9),
        DiscardMedia,
        Read("part-1.md", 3, 5_282),
        Read("part-2.md", 4, 2_604),
        Read("part-3.md", 5, 2_600),
        Write("part-8.md", 10),
    ]);
}

#[test]
fn lecture_recovery_reaches_layout() {
    replay_recovery(&[
        Read("part-0.md", 1, 136),
        View(1),
        Write("part-1.md", 2),
        View(2),
        Read("part-1.md", 2, 2_689),
        Write("part-1.md", 3),
        View(3),
        Write("part-1.md", 4),
        View(4),
        Write("part-1.md", 5),
        View(5),
        DiscardMedia,
        Read("part-1.md", 5, 10_582),
        Write("part-1.md", 6),
        View(6),
        Write("part-2.md", 7),
        View(7),
        Write("part-3.md", 8),
        View(8),
        Write("part-4.md", 9),
        View(9),
        DiscardMedia,
        Read("part-1.md", 6, 13_183),
        Read("part-2.md", 7, 2_607),
        Read("part-3.md", 8, 2_635),
        Read("part-4.md", 9, 2_620),
        Write("part-2.md", 10),
        View(10),
        Write("part-5.md", 11),
        View(11),
        Write("part-6.md", 12),
        View(12),
        Write("part-7.md", 13),
        View(13),
        DiscardMedia,
        Read("part-1.md", 6, 13_183),
        Read("part-2.md", 10, 5_241),
        Read("part-3.md", 8, 2_635),
        Read("part-4.md", 9, 2_620),
        Read("part-7.md", 13, 2_633),
        Read("part-5.md", 11, 2_625),
        Read("part-6.md", 12, 2_624),
        Write("part-8.md", 14),
        View(14),
        Write("part-8.md", 15),
        View(15),
        Write("part-8.md", 16),
        View(16),
        Write("part-8.md", 17),
        View(17),
        DiscardMedia,
        Read("part-8.md", 17, 9_548),
        Write("part-8.md", 18),
        View(18),
        Write("part-8.md", 19),
        Read("part-1.md", 6, 13_183),
        Read("part-2.md", 10, 5_241),
        Read("part-3.md", 8, 2_635),
        Write("layout.json", 20),
    ]);
}

#[test]
fn recovery_exemption_is_bounded_and_only_for_current_run_writes() {
    let owner = RunHandle {
        tenant: "fixture".into(),
        id: "run".into(),
    };
    for writer in [
        owner.clone(),
        RunHandle {
            id: "other".into(),
            ..owner.clone()
        },
    ] {
        let own = writer == owner;
        let namespace = data::Namespace {
            owner: writer,
            agent: "main".into(),
        };
        let write = ToolProgress::Write {
            namespace: namespace.clone(),
            path: "page.md".into(),
            revision: Some(1),
            hash: Some("content".into()),
        };
        let read = ToolProgress::Read {
            namespace,
            path: "page.md".into(),
            revision: 1,
            start: 0,
            end: 100,
        };
        let mut progress = WorkProgress::default();
        let limit = NonZeroU64::new(3);
        progress.apply(&owner, vec![write], limit, "write").unwrap();
        progress
            .apply(&owner, vec![read.clone()], limit, "read")
            .unwrap();
        let epoch = progress.epoch;
        progress.context_changed();
        for i in 1..=3 + usize::from(own) {
            let stopped = progress
                .apply(&owner, vec![read.clone()], limit, &format!("read-{i}"))
                .unwrap();
            assert_eq!(stopped, i == 3 + usize::from(own));
            assert_eq!(progress.epoch, epoch);
        }
    }
}
