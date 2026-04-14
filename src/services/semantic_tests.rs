use super::*;

#[test]
fn semantic_analysis_extracts_summary_and_keywords() {
    let result = SemanticService::analyze(
        "/notes.md",
        b"CoreFS offers journaling, snapshots and content indexing for fast search.",
        &ContentClass::Text,
    );

    assert!(
        result
            .attributes
            .iter()
            .any(|(key, value)| key == "semantic.summary" && value.contains("CoreFS"))
    );
    assert!(
        result
            .attributes
            .iter()
            .any(|(key, value)| key == "semantic.keywords" && value.contains("journaling"))
    );
}
