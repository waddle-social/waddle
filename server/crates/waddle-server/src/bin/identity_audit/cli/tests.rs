use std::fs;

use super::corpus;
use crate::analysis::accounts;
use crate::model::AuditError;
use crate::report::{publish, Report};
use crate::source::Driver;

fn empty_report() -> Report {
    let (rows, collisions) = accounts(Vec::new());
    Report::new(
        Driver::Sqlite,
        "home.example.test".to_owned(),
        rows,
        collisions,
        Vec::new(),
        false,
    )
}

#[test]
fn incomplete_corpus_never_looks_like_no_frames() {
    let directory = tempfile::tempdir().expect("directory");
    let path = directory.path().join("frames.json");
    assert!(matches!(corpus(&path), Err(AuditError::Frames)));
    fs::write(&path, "[\"<message/>\",").expect("invalid corpus");
    assert!(matches!(corpus(&path), Err(AuditError::Frames)));
    fs::write(&path, serde_json::to_vec(&vec![""; 1001]).expect("JSON")).expect("corpus");
    assert!(matches!(corpus(&path), Err(AuditError::Limit)));
}

#[cfg(unix)]
#[test]
fn private_report_cannot_overwrite_or_follow_a_symlink() {
    use std::os::unix::fs::{symlink, PermissionsExt};
    let directory = tempfile::tempdir().expect("directory");
    let path = directory.path().join("audit.json");
    publish(&path, &empty_report()).expect("report");
    assert_eq!(
        fs::metadata(&path).expect("metadata").permissions().mode() & 0o777,
        0o600
    );
    assert!(matches!(
        publish(&path, &empty_report()),
        Err(AuditError::Report)
    ));
    let alias = directory.path().join("alias.json");
    symlink(&path, &alias).expect("symlink");
    let before = fs::read(&path).expect("before");
    assert!(matches!(
        publish(&alias, &empty_report()),
        Err(AuditError::Report)
    ));
    assert_eq!(fs::read(&path).expect("after"), before);
    assert_eq!(fs::read_dir(directory.path()).expect("entries").count(), 2);
}

#[test]
fn invalid_asserted_domain_is_a_finding_even_with_empty_stores() {
    let report = Report::new(
        Driver::Sqlite,
        "xn--bcher-kva.example.test".to_owned(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        false,
    );
    assert!(matches!(
        report.status,
        crate::report::Status::CompleteWithFindings
    ));
    assert!(!report.asserted_domain_findings.is_empty());
}

#[test]
fn corpus_requires_a_regular_file() {
    let directory = tempfile::tempdir().expect("directory");
    assert!(matches!(corpus(directory.path()), Err(AuditError::Frames)));
}
