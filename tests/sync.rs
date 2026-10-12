use std::fs;
use std::path::Path;
use std::process::Command;

use tempfile::tempdir;

fn sync(local: &Path, server: &Path) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_clarra"))
        .arg("sync")
        .arg(local)
        .arg(format!("local:{}", server.display()))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "sync failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn resolve(local: &Path, server: &Path, path: &str, content: &Path) {
    let output = Command::new(env!("CARGO_BIN_EXE_clarra"))
        .arg("resolve")
        .arg(local)
        .arg(format!("local:{}", server.display()))
        .arg(path)
        .arg(content)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "resolve failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn conflict_files(directory: &Path) -> Vec<std::path::PathBuf> {
    fs::read_dir(directory)
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .is_some_and(|name| name.to_string_lossy().contains(".clarra-conflict-"))
        })
        .collect()
}

#[test]
fn devices_sync_offline_cache_conflicts_and_tombstones() {
    let temporary = tempdir().unwrap();
    let server = temporary.path().join("server");
    let alice = temporary.path().join("alice");
    let bob = temporary.path().join("bob");
    fs::create_dir_all(&server).unwrap();
    fs::create_dir_all(&alice).unwrap();
    fs::create_dir_all(&bob).unwrap();
    fs::write(alice.join("plan.md"), "version one").unwrap();

    sync(&alice, &server);
    sync(&bob, &server);
    assert!(!bob.join("plan.md").exists());
    sync(&alice, &server);
    let events = sync(&bob, &server);
    assert_eq!(
        fs::read_to_string(bob.join("plan.md")).unwrap(),
        "version one"
    );
    assert!(events.contains("file_received"));
    assert_eq!(fs::read_dir(server.join(".sync/cache")).unwrap().count(), 0);

    fs::write(alice.join("plan.md"), "alice edit").unwrap();
    fs::write(bob.join("plan.md"), "bob edit").unwrap();
    sync(&alice, &server);
    let bob_events = sync(&bob, &server);
    let alice_events = sync(&alice, &server);
    assert!(bob_events.contains("conflict"));
    assert!(alice_events.contains("conflict"));
    assert_eq!(
        fs::read_to_string(alice.join("plan.md")).unwrap(),
        "alice edit"
    );
    assert_eq!(fs::read_to_string(bob.join("plan.md")).unwrap(), "bob edit");
    assert_eq!(conflict_files(&alice).len(), 1);
    assert_eq!(conflict_files(&bob).len(), 1);

    let merged = temporary.path().join("merged.md");
    fs::write(&merged, "merged").unwrap();
    resolve(&alice, &server, "plan.md", &merged);
    sync(&bob, &server);
    assert_eq!(fs::read_to_string(bob.join("plan.md")).unwrap(), "merged");

    fs::remove_file(alice.join("plan.md")).unwrap();
    fs::write(bob.join("plan.md"), "bob survives deletion").unwrap();
    sync(&alice, &server);
    sync(&bob, &server);
    let alice_events = sync(&alice, &server);
    assert!(alice_events.contains("conflict"));
    assert!(!alice.join("plan.md").exists());
    assert!(conflict_files(&alice).iter().any(|path| {
        fs::read_to_string(path).is_ok_and(|content| content == "bob survives deletion")
    }));
}

#[test]
fn one_cached_object_serves_every_pending_device() {
    let temporary = tempdir().unwrap();
    let server = temporary.path().join("server");
    let alice = temporary.path().join("alice");
    let bob = temporary.path().join("bob");
    let charlie = temporary.path().join("charlie");
    for directory in [&server, &alice, &bob, &charlie] {
        fs::create_dir_all(directory).unwrap();
    }
    fs::write(alice.join("shared.bin"), b"one object").unwrap();

    sync(&alice, &server);
    sync(&bob, &server);
    sync(&charlie, &server);
    sync(&alice, &server);
    assert_eq!(fs::read_dir(server.join(".sync/cache")).unwrap().count(), 1);

    sync(&bob, &server);
    assert_eq!(fs::read_dir(server.join(".sync/cache")).unwrap().count(), 1);
    sync(&charlie, &server);
    assert_eq!(fs::read_dir(server.join(".sync/cache")).unwrap().count(), 0);
    assert_eq!(fs::read(bob.join("shared.bin")).unwrap(), b"one object");
    assert_eq!(fs::read(charlie.join("shared.bin")).unwrap(), b"one object");
}
