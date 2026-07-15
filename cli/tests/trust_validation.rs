//! `sshmux trust` validation + authorized_keys file handling.
//!
//! Security-focused: proves injection attempts are rejected with NO file write,
//! entries are loopback-scoped, dedupe works, and revoke preserves other keys.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use sshmux_cli::trust::{self, AddOutcome};

const KEY_A: &str =
    "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIMd8cW6R+pw+ZQiItqbveNlulKVoiAYJ4qBo2PDAynjX phone";

/// A unique authorized_keys path under cargo's per-binary temp dir.
fn ak_path(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir.join("authorized_keys")
}

#[test]
fn add_creates_scoped_entry_with_0600() {
    let path = ak_path("add_scoped");
    let outcome = trust::add_to_file(&path, KEY_A, "phone", false).unwrap();
    assert_eq!(outcome, AddOutcome::Added);

    let content = fs::read_to_string(&path).unwrap();
    let line = content.lines().next().unwrap();
    assert!(line.starts_with(r#"from="127.0.0.1",restrict ssh-ed25519 "#), "got: {line}");
    assert!(line.ends_with(" sshmux:phone"));
    assert!(content.ends_with('\n'));

    let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "authorized_keys must be created 0600");
}

#[test]
fn dedupe_same_key_no_second_write() {
    let path = ak_path("dedupe");
    assert_eq!(trust::add_to_file(&path, KEY_A, "phone", false).unwrap(), AddOutcome::Added);
    // Same key, different label -> still a duplicate by key material.
    assert_eq!(
        trust::add_to_file(&path, KEY_A, "phone2", false).unwrap(),
        AddOutcome::AlreadyTrusted
    );
    let content = fs::read_to_string(&path).unwrap();
    assert_eq!(content.lines().count(), 1, "duplicate must not be appended");
}

#[test]
fn newline_injection_rejected_no_write() {
    let path = ak_path("inject_newline");
    let evil = format!("{KEY_A}\ncommand=\"rm -rf /\" {KEY_A}");
    assert!(trust::add_to_file(&path, &evil, "phone", false).is_err());
    assert!(!path.exists(), "no file should be created on a rejected key");
}

#[test]
fn options_prefix_injection_rejected() {
    let path = ak_path("inject_options");
    let evil = format!("command=\"x\" {KEY_A}");
    assert!(trust::add_to_file(&path, &evil, "phone", false).is_err());
    assert!(!path.exists());
}

#[test]
fn bad_label_rejected_no_write() {
    let path = ak_path("bad_label");
    assert!(trust::add_to_file(&path, KEY_A, "has space", false).is_err());
    assert!(trust::add_to_file(&path, KEY_A, "has\nnewline", false).is_err());
    assert!(!path.exists());
}

#[test]
fn list_reports_label_and_fingerprint() {
    let path = ak_path("list");
    trust::add_to_file(&path, KEY_A, "phone", false).unwrap();
    let entries = trust::list_file(&path).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].label, "phone");
    assert_eq!(entries[0].algorithm, "ssh-ed25519");
    assert!(entries[0].fingerprint.starts_with("SHA256:"));
}

#[test]
fn untrust_removes_only_matching_and_preserves_others() {
    let path = ak_path("untrust");
    // A pre-existing, non-sshmux line that must survive.
    fs::write(&path, "# my other key\nssh-rsa AAAApreexisting other-comment\n").unwrap();
    trust::add_to_file(&path, KEY_A, "phone", false).unwrap();

    let removed = trust::remove_from_file(&path, "phone").unwrap();
    assert_eq!(removed, 1);

    let content = fs::read_to_string(&path).unwrap();
    assert!(content.contains("# my other key"), "unrelated lines preserved");
    assert!(content.contains("ssh-rsa AAAApreexisting"), "unrelated key preserved");
    assert!(!content.contains("sshmux:phone"), "sshmux entry removed");

    let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "rewrite must keep 0600");

    // Removing a missing label is a no-op.
    assert_eq!(trust::remove_from_file(&path, "phone").unwrap(), 0);
}
