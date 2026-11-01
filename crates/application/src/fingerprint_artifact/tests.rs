use super::*;
use std::process::{Child, Command, Stdio};

const UA: &str = "Mozilla/5.0 Chrome/156.0.0.0";
fn profile() -> ProfileId {
    ProfileId::new("artifact-test").unwrap()
}
fn files() -> [String; 3] {
    [
        stealth_template::render_manifest(profile().as_str()),
        stealth_template::render_content(r#"{"seed":"artifact-test"}"#),
        stealth_template::render_background(),
    ]
}
fn publish(root: &Path) -> PortResult<PathBuf> {
    let [manifest, content, background] = files();
    publish_extension(root, &profile(), &manifest, &content, &background, UA)
}
fn expected(root: &Path) -> PathBuf {
    root.join(digest_hex(profile().as_str().as_bytes()))
        .join(content_hash(&files().map(String::into_bytes), UA))
}
fn staging(root: &Path) -> PathBuf {
    root.join(digest_hex(profile().as_str().as_bytes()))
        .join(".staging")
}
#[derive(Debug)]
struct Root(PathBuf);
impl Root {
    fn new() -> Self {
        let nonce = NEXT_STAGE.fetch_add(1, Ordering::Relaxed);
        let time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "brows-atomic-extension-{}-{time}-{nonce}",
            std::process::id()
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn published_artifact_is_complete_bound_to_profile_and_ua_and_reused_without_rewrite() {
    let root = Root::new();
    let path = publish(&root.0).unwrap();
    assert_eq!(path, expected(&root.0));
    verify_fingerprint_extension(&profile(), &path, UA).unwrap();
    assert_eq!(names(&path).unwrap().len(), 4);
    let before = FILES.map(|name| fs::metadata(path.join(name)).unwrap().modified().unwrap());
    assert_eq!(publish(&root.0).unwrap(), path);
    assert_eq!(
        before,
        FILES.map(|name| fs::metadata(path.join(name)).unwrap().modified().unwrap())
    );
    assert_eq!(fs::read_dir(staging(&root.0)).unwrap().count(), 0);
    assert!(
        verify_fingerprint_extension(&ProfileId::new("another-profile").unwrap(), &path, UA)
            .is_err()
    );
    assert!(verify_fingerprint_extension(&profile(), &path, "another-UA").is_err());
}
#[test]
fn fully_verified_legacy_artifacts_are_reused_but_missing_files_are_never_repaired() {
    let root = Root::new();
    let path = publish(&root.0).unwrap();
    let [manifest, content, background] = files();
    let legacy_hash = digest_hex(format!("{manifest}\0{content}\0{background}\0{UA}").as_bytes());
    assert_eq!(
        path.file_name().unwrap().to_str(),
        Some(legacy_hash.as_str())
    );
    fs::remove_file(path.join(OWNER)).unwrap();
    let before = fs::read(path.join("content.js")).unwrap();
    assert_eq!(publish(&root.0).unwrap(), path);
    assert!(!path.join(OWNER).exists());
    fs::remove_file(path.join("background.js")).unwrap();
    assert!(publish(&root.0).is_err());
    assert!(!path.join("background.js").exists());
    assert_eq!(fs::read(path.join("content.js")).unwrap(), before);
}
#[test]
fn changed_or_extra_files_and_empty_final_directories_are_preserved_and_rejected() {
    for change in [
        "content.js",
        "manifest.json",
        "background.js",
        "extra.js",
        "empty",
    ] {
        let root = Root::new();
        let path = publish(&root.0).unwrap();
        if change == "empty" {
            fs::remove_dir_all(&path).unwrap();
            fs::create_dir(&path).unwrap();
        } else {
            fs::write(path.join(change), b"unexpected").unwrap();
        }
        assert!(verify_fingerprint_extension(&profile(), &path, UA).is_err());
        assert!(publish(&root.0).is_err());
        if change == "empty" {
            assert_eq!(fs::read_dir(path).unwrap().count(), 0);
        } else {
            assert_eq!(fs::read(path.join(change)).unwrap(), b"unexpected");
        }
    }
}
#[test]
fn owner_identity_must_match_and_oversized_files_are_not_read_or_repaired() {
    for broken_owner in [true, false] {
        let root = Root::new();
        let path = publish(&root.0).unwrap();
        if broken_owner {
            fs::write(path.join(OWNER), b"{\"schemaVersion\":2}").unwrap();
        } else {
            File::create(path.join("content.js"))
                .unwrap()
                .set_len(FILE_LIMIT + 1)
                .unwrap();
        }
        assert!(publish(&root.0).is_err());
        assert!(verify_fingerprint_extension(&profile(), &path, UA).is_err());
    }
}
#[test]
fn final_path_is_absent_at_each_staging_boundary_and_owned_incomplete_trees_recover() {
    for count in 0..=3 {
        let root = Root::new();
        let parent = directory(&staging(&root.0), true).unwrap();
        let profile_hash = digest_hex(profile().as_str().as_bytes());
        let artifact = expected(&root.0)
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned();
        let stage = Stage::create(&parent, &profile_hash, &artifact).unwrap();
        for (name, body) in FILES.iter().zip(files()).take(count) {
            write_new(&stage.path.join(name), body.as_bytes()).unwrap();
            assert!(!expected(&root.0).exists());
        }
        let abandoned = stage.path.clone();
        std::mem::forget(stage);
        assert!(!expected(&root.0).exists());
        let path = publish(&root.0).unwrap();
        assert!(!abandoned.exists());
        verify_fingerprint_extension(&profile(), &path, UA).unwrap();
    }
}
#[test]
fn recovery_retains_unowned_malformed_and_unknown_content() {
    let root = Root::new();
    publish(&root.0).unwrap();
    let parent = staging(&root.0);
    let unowned = parent.join("stage-unowned");
    fs::create_dir(&unowned).unwrap();
    fs::write(unowned.join("content.js"), b"keep").unwrap();
    let malformed = parent.join("stage-malformed");
    fs::create_dir(&malformed).unwrap();
    fs::write(malformed.join(OWNER), b"partial json").unwrap();
    let stage = Stage::create(
        &parent,
        &digest_hex(profile().as_str().as_bytes()),
        &"a".repeat(64),
    )
    .unwrap();
    fs::write(stage.path.join("unrecognized-file"), b"keep").unwrap();
    let unknown = stage.path.clone();
    std::mem::forget(stage);
    publish(&root.0).unwrap();
    assert_eq!(fs::read(unowned.join("content.js")).unwrap(), b"keep");
    assert_eq!(fs::read(malformed.join(OWNER)).unwrap(), b"partial json");
    assert_eq!(
        fs::read(unknown.join("unrecognized-file")).unwrap(),
        b"keep"
    );
}
#[test]
fn staging_scan_has_a_fixed_entry_limit() {
    let root = Root::new();
    directory(&staging(&root.0), true).unwrap();
    for i in 0..=STAGE_LIMIT {
        fs::write(staging(&root.0).join(format!("unknown-{i}")), b"keep").unwrap();
    }
    assert!(publish(&root.0).is_err());
    assert!(!expected(&root.0).exists());
    assert_eq!(
        fs::read_dir(staging(&root.0)).unwrap().count(),
        STAGE_LIMIT + 1
    );
}
#[test]
fn concurrent_threads_publish_a_single_complete_artifact() {
    let root = Root::new();
    let barrier = std::sync::Barrier::new(8);
    std::thread::scope(|scope| {
        let children = (0..8)
            .map(|_| {
                scope.spawn(|| {
                    barrier.wait();
                    let path = publish(&root.0).unwrap();
                    verify_fingerprint_extension(&profile(), &path, UA).unwrap();
                    path
                })
            })
            .collect::<Vec<_>>();
        for child in children {
            assert_eq!(child.join().unwrap(), expected(&root.0));
        }
    });
    assert_eq!(fs::read_dir(staging(&root.0)).unwrap().count(), 0);
}
#[test]
fn file_lock_wait_is_bounded_and_a_held_profile_does_not_block_another_profile() {
    let root = Root::new();
    publish(&root.0).unwrap();
    let parent = expected(&root.0).parent().unwrap().to_owned();
    let held = lock(&parent, Duration::from_millis(100)).unwrap();
    let start = Instant::now();
    assert!(lock(&parent, Duration::from_millis(30)).is_err());
    assert!(start.elapsed() < Duration::from_secs(1));
    let other = ProfileId::new("other").unwrap();
    publish_extension(
        &root.0,
        &other,
        &stealth_template::render_manifest(other.as_str()),
        &files()[1],
        &files()[2],
        UA,
    )
    .unwrap();
    drop(held);
    assert!(lock(&parent, Duration::from_millis(30)).is_ok());
}
#[derive(Debug)]
struct ChildGuard(Child);
impl std::ops::Deref for ChildGuard {
    type Target = Child;
    fn deref(&self) -> &Child {
        &self.0
    }
}
impl std::ops::DerefMut for ChildGuard {
    fn deref_mut(&mut self) -> &mut Child {
        &mut self.0
    }
}
impl Drop for ChildGuard {
    fn drop(&mut self) {
        if !matches!(self.0.try_wait(), Ok(Some(_))) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}
fn child(root: &Path, mode: &str, count: usize) -> ChildGuard {
    ChildGuard(
        Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "fingerprint_artifact::tests::publication_child",
                "--ignored",
                "--nocapture",
            ])
            .env("BROWS_ARTIFACT_TEST_ROOT", root)
            .env("BROWS_ARTIFACT_TEST_MODE", mode)
            .env("BROWS_ARTIFACT_TEST_COUNT", count.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap(),
    )
}
fn wait_for(child: &mut Child) -> std::process::ExitStatus {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            let _ = child.wait();
            panic!("publication child exceeded deadline");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}
#[test]
fn different_processes_share_publication_lock_and_reuse_identical_output() {
    let root = Root::new();
    let mut children = (0..4)
        .map(|_| child(&root.0, "publish", 0))
        .collect::<Vec<_>>();
    for process in &mut children {
        assert!(wait_for(process).success());
    }
    verify_fingerprint_extension(&profile(), &expected(&root.0), UA).unwrap();
    assert_eq!(fs::read_dir(staging(&root.0)).unwrap().count(), 0);
}
#[test]
fn killed_writers_release_the_os_lock_and_leave_only_recoverable_staging() {
    for count in 0..=3 {
        let root = Root::new();
        let mut process = child(&root.0, "hold", count);
        let deadline = Instant::now() + Duration::from_secs(10);
        while !root.0.join("ready").exists() {
            assert!(
                process.try_wait().unwrap().is_none(),
                "helper exited before the staging boundary"
            );
            if Instant::now() >= deadline {
                process.kill().unwrap();
                let _ = process.wait();
                panic!("helper never reached staging boundary");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(!expected(&root.0).exists());
        process.kill().unwrap();
        process.wait().unwrap();
        publish(&root.0).unwrap();
        verify_fingerprint_extension(&profile(), &expected(&root.0), UA).unwrap();
        assert_eq!(fs::read_dir(staging(&root.0)).unwrap().count(), 0);
    }
}
#[test]
#[ignore = "subprocess helper invoked by atomic-publication tests"]
fn publication_child() {
    let Some(root) = std::env::var_os("BROWS_ARTIFACT_TEST_ROOT") else {
        return;
    };
    let root = PathBuf::from(root);
    if std::env::var("BROWS_ARTIFACT_TEST_MODE").unwrap() == "publish" {
        publish(&root).unwrap();
        return;
    }
    let parent = directory(&staging(&root), true).unwrap();
    let profile_hash = digest_hex(profile().as_str().as_bytes());
    let _lock = lock(parent.parent().unwrap(), Duration::from_secs(3)).unwrap();
    let artifact = expected(&root)
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    let stage = Stage::create(&parent, &profile_hash, &artifact).unwrap();
    let count = std::env::var("BROWS_ARTIFACT_TEST_COUNT")
        .unwrap()
        .parse::<usize>()
        .unwrap();
    for (name, body) in FILES.iter().zip(files()).take(count) {
        write_new(&stage.path.join(name), body.as_bytes()).unwrap();
    }
    fs::write(root.join("ready"), b"ready").unwrap();
    std::thread::sleep(Duration::from_secs(30));
    drop(stage);
}

#[cfg(unix)]
#[test]
fn links_at_roots_profile_directories_staging_files_and_final_artifacts_are_rejected() {
    use std::os::unix::fs::{symlink, symlink as symlink_dir};
    let root = Root::new();
    let outside = Root::new();
    let linked = root.0.join("linked");
    symlink_dir(&outside.0, &linked).unwrap();
    assert!(publish(&linked).is_err());
    assert_eq!(fs::read_dir(&outside.0).unwrap().count(), 0);
    let profile_hash = digest_hex(profile().as_str().as_bytes());
    symlink_dir(&outside.0, root.0.join(&profile_hash)).unwrap();
    assert!(publish(&root.0).is_err());
    fs::remove_file(root.0.join(&profile_hash)).unwrap();
    let path = publish(&root.0).unwrap();
    fs::remove_file(path.join("content.js")).unwrap();
    let content = outside.0.join("content.js");
    fs::write(&content, b"outside").unwrap();
    symlink(&content, path.join("content.js")).unwrap();
    assert!(publish(&root.0).is_err());
    assert_eq!(fs::read(&content).unwrap(), b"outside");
    fs::remove_file(path.join("content.js")).unwrap();
    fs::remove_dir_all(&path).unwrap();
    symlink_dir(&outside.0, &path).unwrap();
    assert!(publish(&root.0).is_err());
    fs::remove_file(&path).unwrap();
    fs::remove_dir(staging(&root.0)).unwrap();
    symlink_dir(&outside.0, staging(&root.0)).unwrap();
    assert!(publish(&root.0).is_err());
}

#[test]
fn unusual_profile_ids_are_escaped_as_json_and_keep_their_hash_identity() {
    let root = Root::new();
    let profile = ProfileId::new("quotes\"and\\slashes/identity").unwrap();
    let manifest = stealth_template::render_manifest(profile.as_str());
    let value: serde_json::Value = serde_json::from_str(&manifest).unwrap();
    assert_eq!(
        value["name"],
        format!("Brows Stealth for {}", profile.as_str())
    );
    let path =
        publish_extension(&root.0, &profile, &manifest, &files()[1], &files()[2], UA).unwrap();
    verify_fingerprint_extension(&profile, &path, UA).unwrap();
}

#[cfg(windows)]
#[test]
fn windows_junctions_cannot_redirect_extension_publication() {
    let root = Root::new();
    let outside = Root::new();
    let link = root.0.join(digest_hex(profile().as_str().as_bytes()));
    // Junctions do not require symlink privilege. Paths travel through env
    // values, never PowerShell source interpolation or a cmd.exe command line.
    let status=Command::new("powershell.exe").args(["-NoProfile","-NonInteractive","-Command",
        "$ErrorActionPreference='Stop'; New-Item -ItemType Junction -Path $env:BROWS_ARTIFACT_LINK -Value $env:BROWS_ARTIFACT_TARGET | Out-Null"])
        .env("BROWS_ARTIFACT_LINK",&link).env("BROWS_ARTIFACT_TARGET",&outside.0)
        .stdin(Stdio::null()).stdout(Stdio::null()).status().unwrap();
    assert!(
        status.success(),
        "Windows test requires a local NTFS temp directory supporting junctions"
    );
    assert!(publish(&root.0).is_err());
    assert_eq!(fs::read_dir(&outside.0).unwrap().count(), 0);
    fs::remove_dir(link).unwrap();
}
