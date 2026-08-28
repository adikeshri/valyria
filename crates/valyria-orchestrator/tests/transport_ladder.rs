//! D5 transport ladder: a corpus of the malformed / non-standard tool-call
//! shapes real open-weight models emit, plus the bounded reformat-retry
//! loop against the fake runtime.

use serde_json::json;
use valyria_model::{Capabilities, Completion, FinishReason, GenerateRequest, Message, ToolCall};
use valyria_orchestrator::{
    extract, recover_from_text, resolve_tool_calls, Extraction, OrchestratorError,
};
use valyria_runtime_fake::{FakeModelRuntime, Scenario, ScriptedTurn};
use valyria_util::CancellationToken;

fn caps() -> Capabilities {
    Capabilities {
        context_length: 8192,
        supports_native_tools: true,
        supports_grammar: false,
        supports_streaming: true,
    }
}

fn only_call(text: &str) -> ToolCall {
    match recover_from_text(text) {
        Ok(Extraction::Calls(mut calls)) if calls.len() == 1 => calls.pop().unwrap(),
        other => panic!("expected exactly one recovered call from {text:?}, got {other:?}"),
    }
}

#[test]
fn fenced_json_block() {
    let c =
        only_call("```json\n{\"name\": \"read_file\", \"arguments\": {\"path\": \"a.rs\"}}\n```");
    assert_eq!(c.name, "read_file");
    assert_eq!(c.arguments["path"], "a.rs");
}

#[test]
fn fence_without_a_language_tag() {
    let c = only_call("```\n{\"name\": \"x\", \"arguments\": {}}\n```");
    assert_eq!(c.name, "x");
}

#[test]
fn prose_preamble_before_the_object() {
    let c = only_call("Sure, I'll open that file for you.\n{\"name\": \"read_file\", \"arguments\": {\"path\": \"a.rs\"}}");
    assert_eq!(c.name, "read_file");
}

#[test]
fn trailing_comma_is_tolerated() {
    let c = only_call("{\"name\": \"grep\", \"arguments\": {\"pattern\": \"foo\",}}");
    assert_eq!(c.name, "grep");
    assert_eq!(c.arguments["pattern"], "foo");
}

#[test]
fn hermes_qwen_tool_call_tags() {
    let c = only_call(
        "<tool_call>\n{\"name\": \"run\", \"arguments\": {\"cmd\": \"ls\"}}\n</tool_call>",
    );
    assert_eq!(c.name, "run");
    assert_eq!(c.arguments["cmd"], "ls");
}

#[test]
fn mistral_tool_calls_prefix() {
    let c = only_call("[TOOL_CALLS]{\"name\": \"run\", \"arguments\": {}}");
    assert_eq!(c.name, "run");
    assert_eq!(c.arguments, json!({}));
}

#[test]
fn llama_python_tag_with_parameters_key() {
    let c = only_call("<|python_tag|>{\"name\": \"run\", \"parameters\": {\"cmd\": \"ls\"}}");
    assert_eq!(c.name, "run");
    assert_eq!(c.arguments["cmd"], "ls");
}

#[test]
fn tool_and_args_key_aliases() {
    let c = only_call("{\"tool\": \"search\", \"args\": {\"q\": \"x\"}}");
    assert_eq!(c.name, "search");
    assert_eq!(c.arguments["q"], "x");
}

#[test]
fn nested_function_object_with_stringified_arguments() {
    let c = only_call("{\"function\": {\"name\": \"e\", \"arguments\": \"{\\\"a\\\": 1}\"}}");
    assert_eq!(c.name, "e");
    assert_eq!(c.arguments["a"], 1);
}

#[test]
fn nested_tool_call_object() {
    let c = only_call("{\"tool_call\": {\"name\": \"z\", \"arguments\": {}}}");
    assert_eq!(c.name, "z");
}

#[test]
fn arguments_given_as_empty_string() {
    let c = only_call("{\"name\": \"n\", \"arguments\": \"\"}");
    assert_eq!(c.name, "n");
    assert_eq!(c.arguments, json!({}));
}

#[test]
fn array_of_tool_calls() {
    let calls = match recover_from_text(
        "[{\"name\":\"a\",\"arguments\":{}},{\"name\":\"b\",\"arguments\":{}}]",
    ) {
        Ok(Extraction::Calls(c)) => c,
        other => panic!("{other:?}"),
    };
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].name, "a");
    assert_eq!(calls[1].name, "b");
    assert_eq!(calls[0].id, "call_0");
    assert_eq!(calls[1].id, "call_1");
}

#[test]
fn braces_inside_string_literals_do_not_unbalance_the_scan() {
    let c = only_call(
        "{\"name\": \"x\", \"arguments\": {\"brace\": \"}\", \"nested\": {\"a\": [1,2]}}}",
    );
    assert_eq!(c.name, "x");
    assert_eq!(c.arguments["brace"], "}");
    assert_eq!(c.arguments["nested"]["a"][1], 2);
}

#[test]
fn plain_prose_is_not_a_call() {
    assert_eq!(
        recover_from_text("The failing assertion is on line 40 of parser.rs.").unwrap(),
        Extraction::NoCall
    );
}

#[test]
fn a_non_call_json_object_in_prose_is_not_a_call() {
    assert_eq!(
        recover_from_text("Here is the config we loaded: {\"debug\": true, \"level\": 3}").unwrap(),
        Extraction::NoCall
    );
}

#[test]
fn empty_object_is_not_a_call() {
    assert_eq!(recover_from_text("{}").unwrap(), Extraction::NoCall);
}

#[test]
fn wrapped_but_unparseable_is_an_error_not_a_silent_nocall() {
    let err = recover_from_text("<tool_call>{ nope not json </tool_call>").unwrap_err();
    assert!(err.0.to_lowercase().contains("json"));
}

#[test]
fn native_tool_calls_win_over_junk_text() {
    let completion = Completion {
        text: "ignore me {\"name\": \"wrong\"}".into(),
        tool_calls: vec![ToolCall {
            id: "call_native".into(),
            name: "right".into(),
            arguments: json!({}),
        }],
        finish_reason: FinishReason::ToolCalls,
        usage: Default::default(),
    };
    match extract(&completion, &caps()).unwrap() {
        Extraction::Calls(c) => assert_eq!(c[0].name, "right"),
        other => panic!("{other:?}"),
    }
}

// --- bounded reformat-retry against the fake runtime -----------------

#[tokio::test]
async fn reformat_retry_recovers_after_one_bad_turn() {
    let model = FakeModelRuntime::from_scenario(Scenario {
        name: "recover".into(),
        turns: vec![
            ScriptedTurn::Malformed {
                raw: "<tool_call>{ broken".into(),
            },
            ScriptedTurn::ToolCall {
                name: "read_file".into(),
                arguments: json!({"path": "a.rs"}),
            },
        ],
    });
    let req = GenerateRequest::new(vec![Message::user("open a.rs")]).with_turn_hint(0);
    let calls = resolve_tool_calls(&model, req, &CancellationToken::new(), 2)
        .await
        .unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "read_file");
}

#[tokio::test]
async fn reformat_retry_gives_up_after_the_budget() {
    let model = FakeModelRuntime::from_scenario(Scenario {
        name: "never".into(),
        turns: vec![
            ScriptedTurn::Malformed {
                raw: "<tool_call>{ bad 1".into(),
            },
            ScriptedTurn::Malformed {
                raw: "<tool_call>{ bad 2".into(),
            },
            ScriptedTurn::Malformed {
                raw: "<tool_call>{ bad 3".into(),
            },
        ],
    });
    let req = GenerateRequest::new(vec![Message::user("go")]).with_turn_hint(0);
    let err = resolve_tool_calls(&model, req, &CancellationToken::new(), 2)
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        OrchestratorError::UnparseableToolCall { attempts: 3, .. }
    ));
}

#[tokio::test]
async fn a_plain_answer_resolves_to_no_calls() {
    let model = FakeModelRuntime::from_scenario(Scenario {
        name: "answer".into(),
        turns: vec![ScriptedTurn::Finish {
            summary: "the bug is a missing semicolon".into(),
        }],
    });
    let req = GenerateRequest::new(vec![Message::user("what's wrong?")]).with_turn_hint(0);
    let calls = resolve_tool_calls(&model, req, &CancellationToken::new(), 2)
        .await
        .unwrap();
    assert!(calls.is_empty());
}
