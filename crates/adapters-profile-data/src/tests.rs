use super::*;
use std::{
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
fn root(label: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!(
        "brows-storage-{label}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(p.join("profiles")).unwrap();
    p.canonicalize().unwrap()
}
#[test]
fn intents_are_bounded_identity_checked_and_block_launch_until_finished() {
    let root = root("intent");
    let journal = ProfileDataJournal::new(root.join("profiles"), None).unwrap();
    let id = ProfileId::new("profile-a").unwrap();
    let op = DataOperation::new(OperationKind::Migration, &id, "migration-17").unwrap();
    journal.begin(&op).unwrap();
    assert!(journal.begin(&op).is_err());
    assert_eq!(journal.pending().unwrap(), vec![op]);
    assert!(ensure_profile_available(&journal.profile_dir(&id).join("userdata")).is_err());
    journal.finish(&id).unwrap();
    journal.finish(&id).unwrap();
    ensure_profile_available(&journal.profile_dir(&id).join("userdata")).unwrap();
    fs::write(
        journal.profile_dir(&id).join(INTENT_FILE),
        br#"{"version":99}"#,
    )
    .unwrap();
    assert!(journal.pending().is_err());
    assert!(journal.profile_dir(&id).join(INTENT_FILE).exists());
    fs::remove_dir_all(root).unwrap();
}
#[test]
fn lease_survives_clones_and_is_released_only_by_last_owner() {
    let root = root("lease");
    let lease = DataRootLease::acquire(&root).unwrap();
    let copy = lease.clone();
    assert!(DataRootLease::acquire(&root).is_err());
    drop(lease);
    assert!(DataRootLease::acquire(&root).is_err());
    drop(copy);
    drop(DataRootLease::acquire(&root).unwrap());
    fs::remove_dir_all(root).unwrap();
}
#[cfg(unix)]
#[test]
fn interrupted_cleanup_refuses_links_and_preserves_external_data() {
    let root = root("links");
    let outside = root.join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("important"), "keep").unwrap();
    let candidate = root.join("profiles/candidate");
    fs::create_dir(&candidate).unwrap();
    std::os::unix::fs::symlink(&outside, candidate.join("redirect")).unwrap();
    assert!(remove_confined_tree(&candidate, &root.join("profiles")).is_err());
    assert_eq!(
        fs::read_to_string(outside.join("important")).unwrap(),
        "keep"
    );
    fs::remove_dir_all(root).unwrap();
}
#[test]
fn killed_owner_releases_os_lock_but_preserves_intent_for_next_process() {
    let root = root("crash");
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "tests::crash_owner_fixture", "--ignored"])
        .env("BROWS_STORAGE_CRASH_ROOT", &root)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !root.join("ready").exists() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(20));
    }
    let ready = root.join("ready").exists();
    let locked = DataRootLease::acquire(&root).is_err();
    child.kill().unwrap();
    child.wait().unwrap();
    assert!(
        ready && locked,
        "child must own the intent and OS lease before forced termination"
    );
    let lease = DataRootLease::acquire(&root).unwrap();
    let journal = ProfileDataJournal::new(root.join("profiles"), Some(lease)).unwrap();
    assert_eq!(journal.pending().unwrap().len(), 1);
    drop(journal);
    fs::remove_dir_all(root).unwrap();
}
#[test]
#[ignore = "child-only forced-termination fixture"]
fn crash_owner_fixture() {
    let root = PathBuf::from(std::env::var_os("BROWS_STORAGE_CRASH_ROOT").unwrap());
    let journal = ProfileDataJournal::new(
        root.join("profiles"),
        Some(DataRootLease::acquire(&root).unwrap()),
    )
    .unwrap();
    let id = ProfileId::new("crash-profile").unwrap();
    journal
        .begin(&DataOperation::new(OperationKind::Restore, &id, "restore").unwrap())
        .unwrap();
    fs::write(root.join("ready"), "ready").unwrap();
    loop {
        thread::park_timeout(Duration::from_secs(1));
    }
}
