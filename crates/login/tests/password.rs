use login::{Hasher, check_password};

#[test]
fn policy() {
    assert!(check_password(&"a".repeat(9)).is_err());
    assert!(check_password(&"a".repeat(10)).is_ok());
    assert!(check_password(&"a".repeat(128)).is_ok());
    assert!(check_password(&"a".repeat(129)).is_err());
    // Characters, not bytes.
    assert!(check_password(&"é".repeat(10)).is_ok());
}

#[test]
fn hash_verify() {
    let h = Hasher::for_tests();
    let phc = h.hash("correct horse");
    assert!(phc.starts_with("$argon2id$"));
    assert!(h.verify("correct horse", &phc));
    assert!(!h.verify("wrong horse", &phc));
    assert!(!h.verify("correct horse", "not a phc string"));
    assert_ne!(phc, h.hash("correct horse"), "salted");
}
