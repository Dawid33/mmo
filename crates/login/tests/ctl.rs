use login::ctl::{CtlDeps, run_ctl_with};
use login::{Hasher, LoginStore};
use login_api::WorldStatus;

fn args(s: &str) -> Vec<String> {
    s.split_whitespace().map(String::from).collect()
}

fn run(db: &str, line: &str, stdin: &str) -> Result<String, String> {
    let mut out = Vec::new();
    let mut input = stdin.as_bytes();
    let full = format!("--db {db} {line}");
    run_ctl_with(
        &args(&full),
        &mut out,
        CtlDeps {
            hasher: Hasher::for_tests(),
            stdin: &mut input,
        },
    )?;
    Ok(String::from_utf8(out).unwrap())
}

fn temp_dir(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("login-ctl-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn keygen_numbers_files_and_signs_with_newest() {
    let root = temp_dir("keygen");
    let keys = root.join("keys"); // does not exist yet
    let db = root.join("l.sqlite");
    let line = format!("keygen --dir {}", keys.display());
    let kid1 = run(db.to_str().unwrap(), &line, "").unwrap();
    let kid2 = run(db.to_str().unwrap(), &line, "").unwrap();
    assert_ne!(kid1.trim(), kid2.trim());
    let mut names: Vec<_> = std::fs::read_dir(&keys)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    assert_eq!(names, ["0001.pem", "0002.pem"]);
    // Never prints key material.
    assert!(!kid1.contains("PRIVATE"));
    let signer = auth::Signer::load_dir(&keys).unwrap();
    assert_eq!(signer.signing_key().kid(), kid2.trim());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(keys.join("0002.pem")).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test(flavor = "multi_thread")]
async fn world_add_list_remove() {
    let root = temp_dir("world");
    let db = root.join("l.sqlite");
    let db = db.to_str().unwrap().to_string();
    let d = db.clone();
    tokio::task::spawn_blocking(move || {
        run(&d, "world add dev --name Dev --quic 127.0.0.1:6466 --wt https://127.0.0.1:6467/", "").unwrap();
        let list = run(&d, "world list", "").unwrap();
        assert!(list.contains("dev") && list.contains("open") && list.contains("127.0.0.1:6466"), "{list}");
        run(&d, "world close dev", "").unwrap();
        assert!(run(&d, "world list", "").unwrap().contains("closed"));
        run(&d, "world open dev", "").unwrap();
        assert!(run(&d, "world list", "").unwrap().contains("open"));
        assert!(run(&d, "world close nope", "").is_err());
        run(&d, "world remove dev", "").unwrap();
        assert!(run(&d, "world remove dev", "").is_err());
        assert!(!run(&d, "world list", "").unwrap().contains("dev"));
    })
    .await
    .unwrap();
    let store = LoginStore::open(&db).await.unwrap();
    assert!(store.worlds().await.unwrap().is_empty());
    let _ = WorldStatus::Open;
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test(flavor = "multi_thread")]
async fn set_password_then_login() {
    let root = temp_dir("pw");
    let db = root.join("l.sqlite");
    let db = db.to_str().unwrap().to_string();
    let store = LoginStore::open(&db).await.unwrap();
    store.create_account("a@b.c", "old", 0).await.unwrap().unwrap();
    drop(store);
    let d = db.clone();
    tokio::task::spawn_blocking(move || {
        assert!(run(&d, "account set-password nobody@x.y", "correct horse battery\n").is_err());
        assert!(run(&d, "account set-password A@B.c", "short\n").is_err());
        let out = run(&d, "account set-password A@B.c", "correct horse battery\n").unwrap();
        assert!(!out.contains("correct horse"));
        run(&d, "account verify a@b.c", "").unwrap();
    })
    .await
    .unwrap();
    let store = LoginStore::open(&db).await.unwrap();
    let row = store.account_by_email("a@b.c").await.unwrap().unwrap();
    assert!(Hasher::for_tests().verify("correct horse battery", &row.password_hash));
    assert!(row.email_verified_at.is_some());
    let _ = std::fs::remove_dir_all(&root);
}
