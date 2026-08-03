#![no_main]

use code_system_graph_core::{
    ChangeRequest, ContractRequest, DoctorRequest, ExportRequest, ImpactRequest, PullRequestListRequest, SearchRequest, TraversalRequest
};
use libfuzzer_sys::fuzz_target;

const MAX_FUZZ_INPUT_BYTES: usize = 2 * 1024 * 1024;

fuzz_target!(|data: &[u8]| {
    if data.len() > MAX_FUZZ_INPUT_BYTES {
        return;
    }

    let _ = serde_json::from_slice::<ChangeRequest>(data);
    let _ = serde_json::from_slice::<ContractRequest>(data);
    let _ = serde_json::from_slice::<DoctorRequest>(data);
    let _ = serde_json::from_slice::<ExportRequest>(data);
    let _ = serde_json::from_slice::<ImpactRequest>(data);
    let _ = serde_json::from_slice::<PullRequestListRequest>(data);
    let _ = serde_json::from_slice::<SearchRequest>(data);
    let _ = serde_json::from_slice::<TraversalRequest>(data);
});
