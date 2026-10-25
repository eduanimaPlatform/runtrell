// SPDX-License-Identifier: Apache-2.0
use std::{collections::BTreeSet, error::Error, fs, path::Path};

pub fn names(suite: &str, feature: &str) -> Result<Vec<&'static str>, Box<dyn Error>> {
    if !["default", "python"].contains(&feature) {
        return Err("feature must be default or python".into());
    }
    let mut names = match suite {
        "linux" => vec![
            "linux_limits_and_boundaries",
            "linux_cancel_descendants_and_recover_without_registry",
            "linux_backend_process_crash",
            "linux_non_cgroup_directory_is_rejected_before_resource_acquisition",
            "linux_late_helper_cannot_join_after_stop_marker",
            "linux_payload_has_no_privileges",
            "linux_concurrent_runs_reclaim_independently",
        ],
        "macos" if feature == "default" => {
            vec!["real_isolation_loop_output_fork_env_and_workspace_boundaries"]
        }
        _ => return Err("unsupported isolation suite/feature".into()),
    };
    if suite == "linux" && feature == "python" {
        names.push("linux_python_workspace_output_is_saved_before_cleanup");
    }
    names.sort();
    Ok(names)
}

fn passed(name: &str, log: &str) -> bool {
    // Each parent test has its own --exact invocation, with captured libtest output.
    // Child-process stderr, 'running N tests', and incomplete child harness output
    // cannot count as a completed parent result. No --nocapture/--show-output.
    let expected = format!("test {name} ... ok");
    let results: Vec<_> = log
        .lines()
        .filter(|l| l.starts_with("test ") && !l.starts_with("test result:"))
        .collect();
    results == [expected.as_str()] && {
        let summaries: Vec<_> = log
            .lines()
            .filter(|l| l.starts_with("test result:"))
            .collect();
        summaries.len() == 1
            && summaries[0]
                .strip_prefix("test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; ")
                .and_then(|tail| tail.split_once(" filtered out; finished in "))
                .is_some_and(|(filtered, elapsed)| {
                    filtered.parse::<usize>().is_ok() && elapsed.ends_with('s')
                })
    }
}

pub fn verify(suite: &str, feature: &str, dir: &Path) -> Result<(), Box<dyn Error>> {
    let expected = names(suite, feature)?;
    let listed = fs::read_to_string(dir.join("list.txt"))?;
    let listed: BTreeSet<_> = listed
        .lines()
        .filter_map(|l| l.strip_suffix(": test"))
        .collect();
    if listed != expected.iter().copied().collect() {
        return Err(
            format!("{suite}/{feature}: test inventory differs from required names").into(),
        );
    }
    for name in &expected {
        let log = fs::read_to_string(dir.join(format!("{name}.log")))?;
        if !passed(name, &log) {
            return Err(format!(
                "{suite}/{feature}: required test did not run exactly once and pass: {name}"
            )
            .into());
        }
    }
    println!(
        "Verified {suite}/{feature}: {} required named tests, all ok, none skipped",
        expected.len()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn log(name: &str) -> String {
        format!(
            "running 1 test\ntest {name} ... ok\n\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 5 filtered out; finished in 1.23s\n"
        )
    }
    #[test]
    fn checks_named_parent_results_and_summary_without_counting_child_stderr() {
        let good = log("parent");
        assert!(passed("parent", &good));
        assert!(passed(
            "parent",
            &format!("Error: \"CANCELLED_BEFORE_JOIN\"\n{good}")
        ));
        for bad in [
            good.replace("parent", "another"),
            good.replace("... ok", "... ignored"),
            good.replace("... ok", "... FAILED"),
            good.replace(
                "1 passed; 0 failed; 0 ignored",
                "0 passed; 0 failed; 1 ignored",
            ),
            good.replace("1 passed", "0 passed"),
            format!("{good}{good}"),
            "running 0 tests\n".into(),
        ] {
            assert!(
                !passed("parent", &bad),
                "accepted incomplete/incorrect parent result: {bad}"
            );
        }
    }
    #[test]
    fn expected_inventory_depends_on_feature() {
        assert_eq!(names("linux", "default").unwrap().len(), 7);
        assert_eq!(names("linux", "python").unwrap().len(), 8);
        assert_eq!(names("macos", "default").unwrap().len(), 1);
        assert!(names("linux", "unknown").is_err());
    }
}
