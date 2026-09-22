use trellis_rs::jobs::keys::job_key;

#[test]
fn jobs_keys_match_expected_wire_format() {
    assert_eq!(
        job_key("documents", "document-process", "job-1"),
        "documents.document-process.job-1"
    );
}
