use super::*;

#[test]
fn normalize_drive_letter_accepts_common_forms() {
    assert_eq!(normalize_drive_letter("x").expect("plain letter"), 'X');
    assert_eq!(normalize_drive_letter("x:").expect("colon"), 'X');
    assert_eq!(normalize_drive_letter("X:\\").expect("root slash"), 'X');
}

#[test]
fn normalize_drive_letter_rejects_invalid_designators() {
    assert!(normalize_drive_letter("").is_err());
    assert!(normalize_drive_letter("xy").is_err());
    assert!(normalize_drive_letter("1:").is_err());
}
