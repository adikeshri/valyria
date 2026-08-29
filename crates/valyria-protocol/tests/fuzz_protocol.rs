//! Property fuzzing for the protocol decoder (§7: "protocol decoder ...
//! proptest"; §4.27). Two invariants:
//!
//! 1. **Total** — arbitrary bytes fed to the frame decoder produce `Ok`
//!    or `Err`, never a panic.
//! 2. **Lossless** — any `Request` this build can construct survives
//!    `encode_line` -> `from_str` unchanged (the newline-delimited framing
//!    the daemon relies on).

use proptest::prelude::*;
use valyria_protocol::messages::{
    Empty, EventsSubscribeRequest, HelloRequest, MemoryListRequest, StoragePurgeRequest,
    TaskCreateRequest, TaskIdRequest, TaskStatusRequest,
};
use valyria_protocol::transport::{encode_line, ClientFrame};
use valyria_protocol::Request;

fn arb_request() -> impl Strategy<Value = Request> {
    prop_oneof![
        "[a-zA-Z0-9 _-]{0,40}".prop_map(|client_name| Request::Hello(HelloRequest { client_name })),
        ".{0,120}".prop_map(|objective| Request::TaskCreate(TaskCreateRequest { objective })),
        "[a-z0-9_]{0,30}".prop_map(|id| Request::TaskStatus(TaskStatusRequest { task_id: id })),
        "[a-z0-9_]{0,30}".prop_map(|id| Request::TaskReport(TaskIdRequest { task_id: id })),
        "[a-z0-9_]{0,30}".prop_map(|id| Request::TaskCancel(TaskIdRequest { task_id: id })),
        any::<u64>().prop_map(|since| Request::EventsSubscribe(EventsSubscribeRequest { since })),
        ".{0,40}".prop_map(|q| Request::MemoryList(MemoryListRequest {
            query: Some(q),
            limit: None
        })),
        ".{0,20}".prop_map(|scope| Request::StoragePurge(StoragePurgeRequest {
            scope,
            dry_run: true
        })),
        Just(Request::TaskList(Empty {})),
        Just(Request::DoctorRun(Empty {})),
        Just(Request::ModelList(Empty {})),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 600, ..ProptestConfig::default() })]

    #[test]
    fn arbitrary_bytes_never_panic_the_frame_decoder(raw in prop::collection::vec(any::<u8>(), 0..512)) {
        let text = String::from_utf8_lossy(&raw);
        let _ = serde_json::from_str::<ClientFrame>(&text);
    }

    #[test]
    fn arbitrary_ascii_lines_never_panic_the_frame_decoder(line in ".{0,300}") {
        let _ = serde_json::from_str::<ClientFrame>(&line);
    }

    #[test]
    fn every_constructible_request_round_trips_through_the_framing(req in arb_request()) {
        let frame = ClientFrame::Call(req.clone());
        let line = encode_line(&frame);
        prop_assert!(line.ends_with('\n'));
        prop_assert!(!line.trim_end().contains('\n'));
        let back: ClientFrame = serde_json::from_str(line.trim_end()).expect("re-decodes");
        prop_assert_eq!(ClientFrame::Call(req), back);
    }
}
