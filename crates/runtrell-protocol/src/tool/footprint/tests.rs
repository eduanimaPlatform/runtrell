// Derived from Rig v0.45.0 (98ed06d7f1c911e6e4c7fdb5d690093bc964c514), crates/rig-core/src/tool/footprint/tests.rs.
// Copyright (c) 2024, Playgrounds Analytics Inc.
// Modified for runtrell; inherited code: MIT (LICENSES/MIT-Rig.txt); changes: Apache-2.0.
use std::path::PathBuf;

use super::{Scope, Touches};
use crate::completion::ToolAnnotations;

fn paths(paths: &[&str]) -> Scope {
    Scope::Paths(paths.iter().map(PathBuf::from).collect())
}

/// Two calls conflict when one writes what the other reads or writes:
/// paths meet when one is the other or under it, `Anything` meets all,
/// and `Independent` reads anything.
#[test]
fn the_conflict_table() {
    let read_a = Touches::Reads(paths(&["/w/a.rs"]));
    let read_b = Touches::Reads(paths(&["/w/b.rs"]));
    let write_a = Touches::Writes(paths(&["/w/a.rs"]));
    let write_b = Touches::Writes(paths(&["/w/b.rs"]));
    let read_dir = Touches::Reads(paths(&["/w"]));
    let read_sibling = Touches::Reads(paths(&["/w/a.rs.bak", "/x"]));
    let read_all = Touches::Reads(Scope::Anything);
    let write_all = Touches::Writes(Scope::Anything);
    let independent = Touches::Independent;

    let cases = [
        (&read_a, &read_a, false),
        (&read_a, &read_all, false),
        (&read_all, &read_all, false),
        (&read_a, &write_a, true),
        (&read_b, &write_a, false),
        (&write_a, &write_a, true),
        (&write_a, &write_b, false),
        (&write_a, &read_dir, true),
        (&write_a, &read_sibling, false),
        (&write_all, &read_b, true),
        (&write_all, &write_b, true),
        (&independent, &write_a, true),
        (&independent, &write_all, true),
        (&independent, &read_a, false),
        (&independent, &independent, false),
    ];
    for (one, other, conflict) in cases {
        assert_eq!(one.conflicts_with(other), conflict, "{one:?} / {other:?}");
        assert_eq!(other.conflicts_with(one), conflict, "{other:?} / {one:?}");
    }
}

/// Only `Reads` only reads; a tool's annotations give a coarse default.
#[test]
fn reads_only_and_the_default_from_annotations() {
    assert!(Touches::Reads(Scope::Anything).reads_only());
    assert!(!Touches::Writes(paths(&["/w"])).reads_only());
    assert!(!Touches::Independent.reads_only());
    let read_only = ToolAnnotations::default().with_read_only(true);
    assert_eq!(
        Touches::from_annotations(&read_only),
        Touches::Reads(Scope::Anything)
    );
    assert_eq!(
        Touches::from_annotations(&ToolAnnotations::default()),
        Touches::Writes(Scope::Anything)
    );
    // A remote server's word that a tool only reads never makes its calls
    // run again after a restart.
    assert_eq!(Touches::from_hints(&read_only), Touches::Independent);
    assert_eq!(
        Touches::from_hints(&ToolAnnotations::default()),
        Touches::Writes(Scope::Anything)
    );
}

/// `..` and `.` fold away, so a path cannot climb past what a guard
/// compares; a relative path, which names no checkable place, is anything.
#[test]
fn normalized_folds_dots_and_widens_relative_paths() {
    let climbing = Touches::Writes(paths(&["/work/src/../../etc/./passwd", "/work/a.rs"]));
    assert_eq!(
        climbing.normalized(),
        Touches::Writes(paths(&["/etc/passwd", "/work/a.rs"]))
    );
    assert_eq!(
        Touches::Reads(paths(&["/../x"])).normalized(),
        Touches::Reads(paths(&["/x"]))
    );
    assert_eq!(
        Touches::Writes(paths(&["/work/a.rs", "notes.txt"])).normalized(),
        Touches::Writes(Scope::Anything)
    );
    assert_eq!(Touches::Independent.normalized(), Touches::Independent);
}

/// A host adapter consumes the declared footprint without the upstream serve layer.
#[test]
fn a_tool_handler_gives_each_call_its_footprint() {
    let definition = crate::completion::ToolDefinition::new(
        crate::message::ToolName::new("open").expect("name"),
        "Opens a file.",
        serde_json::json!({"type":"string"}),
    );
    let footprint = |name: &str, args: &str| {
        if name != definition.name.as_str() {
            return None;
        }
        let path: PathBuf = serde_json::from_str(args).ok()?;
        Some(Touches::Reads(Scope::Paths(vec![path])))
    };
    assert_eq!(
        footprint("open", "\"/w/a.rs\""),
        Some(Touches::Reads(paths(&["/w/a.rs"])))
    );
    assert_eq!(footprint("open", "{"), None);
    assert_eq!(footprint("other", "\"/w/a.rs\""), None);
}
