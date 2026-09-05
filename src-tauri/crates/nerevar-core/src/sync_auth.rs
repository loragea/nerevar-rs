//! Authentication for the sync HTTP server: a single shared password, taken
//! from the TES3MP server's `[General] password`, presented by clients in the
//! [`SYNC_PASSWORD_HEADER`] header. See `docs/headless-hosting.md` (§4, "The
//! Nerevar config file") for the operator's view.

pub const SYNC_PASSWORD_HEADER: &str = "X-Nerevar-Sync-Password";

pub fn sync_password_required(expected: &str) -> bool {
    !expected.is_empty()
}

/// Whether `provided` authenticates against `expected`.
///
/// An empty `expected` means the host set no password and accepts everything,
/// including a request that sends no header at all — documented behaviour, not
/// an oversight (`docs/headless-hosting.md`: "An empty password means anyone
/// who can reach the sync port can download the mod list").
///
/// When a password *is* set, the comparison is constant-time in the contents
/// of both strings: it examines every byte of the longer of the two and folds
/// the length difference into the same accumulator, so a caller learns nothing
/// about how far a wrong guess got. The number of iterations still depends on
/// the length of the longer string, which the caller already controls; only
/// the password's bytes are protected, which is the property that matters
/// against an attacker probing the header a byte at a time.
pub fn sync_password_matches(expected: &str, provided: Option<&str>) -> bool {
    if expected.is_empty() {
        return true;
    }
    match provided {
        Some(value) => constant_time_eq(value.as_bytes(), expected.as_bytes()),
        None => false,
    }
}

/// Byte equality with no early exit and no data-dependent branching.
///
/// Deliberately dependency-free (core takes no new crates for this): the
/// unequal-length case is handled by XOR-ing the lengths into the accumulator
/// and reading missing bytes as zero, so `"ab"` and `"ab\0"` still differ.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    let mut diff: usize = a.len() ^ b.len();
    for index in 0..a.len().max(b.len()) {
        let left = a.get(index).copied().unwrap_or(0);
        let right = b.get(index).copied().unwrap_or(0);
        diff |= usize::from(left ^ right);
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn equal_passwords_match() {
        assert!(sync_password_matches("hunter2", Some("hunter2")));
    }

    #[test]
    fn unequal_same_length_passwords_do_not_match() {
        assert!(!sync_password_matches("hunter2", Some("hunter3")));
        // Differing only in the last byte must be as rejected as differing in
        // the first — the point of not exiting early.
        assert!(!sync_password_matches("hunter2", Some("Xunter2")));
    }

    #[test]
    fn unequal_different_length_passwords_do_not_match() {
        assert!(!sync_password_matches("hunter2", Some("hunter")));
        assert!(!sync_password_matches("hunter2", Some("hunter22")));
        assert!(!sync_password_matches("hunter2", Some("")));
        // A zero byte must not pass for "absent byte".
        assert!(!sync_password_matches("ab", Some("ab\0")));
        assert!(!sync_password_matches("ab\0", Some("ab")));
    }

    #[test]
    fn an_empty_expected_password_accepts_everything() {
        assert!(sync_password_matches("", Some("anything")));
        assert!(sync_password_matches("", Some("")));
        assert!(sync_password_matches("", None));
        assert!(!sync_password_required(""));
    }

    #[test]
    fn a_set_password_rejects_a_missing_header() {
        assert!(!sync_password_matches("hunter2", None));
        assert!(sync_password_required("hunter2"));
    }

    #[test]
    fn constant_time_eq_agrees_with_plain_equality() {
        let cases = [
            ("", ""),
            ("a", ""),
            ("", "a"),
            ("abc", "abc"),
            ("abc", "abd"),
            ("abc", "abcd"),
            ("\0", ""),
            ("héllo", "héllo"),
            ("héllo", "hello"),
        ];
        for (left, right) in cases {
            assert_eq!(
                constant_time_eq(left.as_bytes(), right.as_bytes()),
                left == right,
                "mismatch for {left:?} vs {right:?}"
            );
        }
    }
}
