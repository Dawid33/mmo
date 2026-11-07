use login::{Hasher, check_password, normalize_email};

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
fn email_normalized() {
    assert_eq!(normalize_email(" A@B.c ").unwrap(), "a@b.c");
    for bad in ["a@@b", "@b", "a@", "ab", "a@b@c", ""] {
        assert!(normalize_email(bad).is_err(), "{bad:?}");
    }
    let long = format!("{}@x.y", "a".repeat(251));
    assert!(normalize_email(&long).is_err());
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
