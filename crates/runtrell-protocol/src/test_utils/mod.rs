// Derived from Rig v0.45.0 (98ed06d7f1c911e6e4c7fdb5d690093bc964c514), crates/rig-core/src/test_utils/completion.rs.
// Copyright (c) 2024, Playgrounds Analytics Inc.
// Modified for runtrell; inherited code: MIT (LICENSES/MIT-Rig.txt); changes: Apache-2.0.
mod trace_capture;
pub use trace_capture::TraceCapture;
/// Every set option refused, for a test target that takes none.
pub fn refuse_options(
    fields: crate::completion::options::OptionFields<'_>,
) -> crate::completion::options::OptionMap {
    each_set(fields, || {
        crate::completion::options::Mapping::unsupported("the test target takes no options")
    })
}

/// `answer` for every set option but the cache key, which is unrouted.
fn each_set(
    fields: crate::completion::options::OptionFields<'_>,
    answer: impl Fn() -> crate::completion::options::Mapping,
) -> crate::completion::options::OptionMap {
    use crate::completion::options::{Mapping, OptionMap};
    let [
        reasoning,
        cache,
        service_tier,
        verbosity,
        parallel_tool_calls,
        top_p,
        seed,
        stop,
        _,
    ] = fields
        .set()
        .map(|set| if set { answer() } else { Mapping::Nothing });
    OptionMap {
        reasoning,
        cache,
        service_tier,
        verbosity,
        parallel_tool_calls,
        top_p,
        seed,
        stop,
        cache_key: Mapping::unrouted(fields.cache_key),
    }
}

#[derive(Debug)]
pub struct Target;
impl crate::completion::ReplayTarget for Target {
    fn api(&self) -> crate::message::Api {
        crate::message::Api::from_static("test")
    }
    fn provider(&self) -> &str {
        "test"
    }
    fn model(&self) -> &str {
        "test"
    }
    fn accepts(&self, _: &str) -> crate::completion::Accepts {
        crate::completion::Accepts::ALL
    }
    fn map_options(
        &self,
        _: &crate::completion::CompletionRequest,
        fields: crate::completion::options::OptionFields<'_>,
    ) -> crate::completion::options::OptionMap {
        refuse_options(fields)
    }
}
