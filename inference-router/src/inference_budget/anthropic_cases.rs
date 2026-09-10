// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

pub fn incomplete() -> Vec<(&'static str, String)> {
    let start = r#"data: {"type":"message_start","message":{"usage":{"input_tokens":2,"output_tokens":0}}}

"#;
    let content = r#"data: {"type":"content_block_delta","delta":{"type":"text_delta","text":"generated"}}

"#;
    let stop = "data: {\"type\":\"message_stop\"}\n\n";
    let final_usage = r#"data: {"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":7}}

"#;
    [
        ("missing-final", format!("{start}{content}{stop}")),
        ("no-content-missing-final", format!("{start}{stop}")),
        ("empty-usage", format!("{start}{content}data: {{\"type\":\"message_delta\",\"delta\":{{\"stop_reason\":\"end_turn\"}},\"usage\":{{}}}}\n\n{stop}")),
        ("missing-usage", format!("{start}{content}data: {{\"type\":\"message_delta\",\"delta\":{{\"stop_reason\":\"end_turn\"}}}}\n\n{stop}")),
        ("malformed-output", format!("{start}{content}data: {{\"type\":\"message_delta\",\"delta\":{{\"stop_reason\":\"end_turn\"}},\"usage\":{{\"output_tokens\":\"7\"}}}}\n\n{stop}")),
        ("negative-output", format!("{start}{content}data: {{\"type\":\"message_delta\",\"delta\":{{\"stop_reason\":\"end_turn\"}},\"usage\":{{\"output_tokens\":-1}}}}\n\n{stop}")),
        ("intermediate-only", format!("{start}{content}data: {{\"type\":\"message_delta\",\"usage\":{{\"output_tokens\":7}}}}\n\n{stop}")),
        ("changed-input", format!("{start}{content}data: {{\"type\":\"message_delta\",\"delta\":{{\"stop_reason\":\"end_turn\"}},\"usage\":{{\"input_tokens\":3,\"output_tokens\":7}}}}\n\n{stop}")),
        ("zero-output-with-content", format!("{start}{content}data: {{\"type\":\"message_delta\",\"delta\":{{\"stop_reason\":\"end_turn\"}},\"usage\":{{\"output_tokens\":0}}}}\n\n{stop}")),
        ("decreasing-output", format!("{start}{content}data: {{\"type\":\"message_delta\",\"usage\":{{\"output_tokens\":8}}}}\n\n{final_usage}{stop}")),
        ("duplicate-final", format!("{start}{content}{final_usage}{final_usage}{stop}")),
        ("content-after-final", format!("{start}{final_usage}{content}{stop}")),
        ("start-reset", format!("{start}{content}{start}{final_usage}{stop}")),
        ("no-terminal", format!("{start}{content}{final_usage}")),
        ("final-after-stop", format!("{start}{content}{stop}{final_usage}")),
        ("invalid-stop-reason", format!("{start}{content}data: {{\"type\":\"message_delta\",\"delta\":{{\"stop_reason\":42}},\"usage\":{{\"output_tokens\":7}}}}\n\n{stop}")),
    ].into_iter().collect()
}

pub fn complete() -> String {
    concat!(
        "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":2,\"output_tokens\":0}}}\n\n",
        "data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"generated\"}}\n\n",
        "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":7}}\n\n",
        "data: {\"type\":\"message_stop\"}\n\n",
    ).into()
}

#[test]
fn refunds_require_a_consistent_final_delta_and_complete_terminal_at_every_chunk_boundary() {
    use super::usage::StreamUsage;
    use crate::inference_budget_contract::tariffs::Operation;
    for (name, wire) in incomplete() {
        for size in [1, 2, 7, wire.len()] {
            let mut parser = StreamUsage::new(Operation::AnthropicMessages);
            for chunk in wire.as_bytes().chunks(size) {
                parser.push(chunk);
            }
            assert!(parser.finish().is_none(), "{name}, chunk size {size}");
        }
    }
    for size in 1..complete().len() {
        let mut parser = StreamUsage::new(Operation::AnthropicMessages);
        for chunk in complete().as_bytes().chunks(size) {
            parser.push(chunk);
        }
        let usage = parser.finish().unwrap();
        assert_eq!((usage.input_tokens, usage.output_tokens), (2, 7));
    }
}
