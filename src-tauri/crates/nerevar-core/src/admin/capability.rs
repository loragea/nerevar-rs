//! Roles as data.
//!
//! An `/admin` route names the [`Capability`] it needs; an authenticated
//! caller carries a *role name* read from `admins.json`. [`ROLE_TABLE`] is the
//! single place that maps one to the other, so adding a role (a `contributor`
//! that may stage but not apply, say) is a table entry rather than a new
//! branch in every handler. Nothing in this crate is allowed to ask "is this
//! caller an admin?" — only "does this caller's role grant this capability?".

/// One thing an `/admin` caller may be permitted to do.
///
/// Capabilities for routes that do not exist yet (`Stage`, `Apply`,
/// `Restart`) are declared now so the table is complete and the later
/// milestones only add routes, not vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Capability {
    /// Read the host's state: `GET /admin/status`.
    Status,
    /// Upload packages and edit the pending load order.
    Stage,
    /// Apply staged changes: rewrite `data/`, rebuild the manifest.
    Apply,
    /// Restart the TES3MP dedicated server.
    Restart,
    /// Create and revoke other admins.
    ManageAdmins,
}

impl Capability {
    /// Lower-case name used in log lines and error bodies.
    pub fn as_str(self) -> &'static str {
        match self {
            Capability::Status => "status",
            Capability::Stage => "stage",
            Capability::Apply => "apply",
            Capability::Restart => "restart",
            Capability::ManageAdmins => "manage-admins",
        }
    }
}

impl std::fmt::Display for Capability {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The only role that exists today: every capability.
pub const ROLE_ADMIN: &str = "admin";

/// Role name → the capabilities that role grants. The whole authorization
/// model.
const ROLE_TABLE: &[(&str, &[Capability])] = &[(
    ROLE_ADMIN,
    &[
        Capability::Status,
        Capability::Stage,
        Capability::Apply,
        Capability::Restart,
        Capability::ManageAdmins,
    ],
)];

/// A role that grants only [`Capability::Status`], so the `403` branch of the
/// capability guard can be exercised end to end while `admin` is still the
/// only role that ships.
///
/// Compiled out of release builds entirely (`cfg(test)` for this crate's own
/// tests, the `test-util` feature for `tests/`), so it is not a real role a
/// host could ever be configured with — an `admins.json` naming it on a
/// shipped binary resolves to an unknown role and authenticates nothing,
/// exactly like any other typo. The name is deliberately unusable-looking.
#[cfg(any(test, feature = "test-util"))]
pub const ROLE_TEST_STATUS_ONLY: &str = "nerevar-test-status-only";

#[cfg(any(test, feature = "test-util"))]
const TEST_ROLE_TABLE: &[(&str, &[Capability])] = &[(ROLE_TEST_STATUS_ONLY, &[Capability::Status])];

#[cfg(any(test, feature = "test-util"))]
fn test_capabilities_for_role(role: &str) -> Option<&'static [Capability]> {
    TEST_ROLE_TABLE
        .iter()
        .find(|(name, _)| *name == role)
        .map(|(_, capabilities)| *capabilities)
}

#[cfg(not(any(test, feature = "test-util")))]
fn test_capabilities_for_role(_role: &str) -> Option<&'static [Capability]> {
    None
}

/// The capabilities `role` grants, or `None` when the name is not in the
/// table. An unknown role is not an error here — it is a hand-edited or
/// downgrade-stranded `admins.json`, and the caller decides what to do (the
/// store refuses to authenticate such a record and warns once).
pub fn capabilities_for_role(role: &str) -> Option<&'static [Capability]> {
    ROLE_TABLE
        .iter()
        .find(|(name, _)| *name == role)
        .map(|(_, capabilities)| *capabilities)
        .or_else(|| test_capabilities_for_role(role))
}

/// Whether `role` appears in the table at all.
pub fn role_is_known(role: &str) -> bool {
    capabilities_for_role(role).is_some()
}

/// Whether `role` grants `capability`. An unknown role grants nothing.
pub fn role_grants(role: &str, capability: Capability) -> bool {
    capabilities_for_role(role).is_some_and(|caps| caps.contains(&capability))
}

/// Every role name the table knows, for error messages and `--help` text.
pub fn known_role_names() -> Vec<&'static str> {
    ROLE_TABLE.iter().map(|(name, _)| *name).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_admin_role_grants_every_capability() {
        for capability in [
            Capability::Status,
            Capability::Stage,
            Capability::Apply,
            Capability::Restart,
            Capability::ManageAdmins,
        ] {
            assert!(
                role_grants(ROLE_ADMIN, capability),
                "admin should grant {capability}"
            );
        }
    }

    /// The narrow role exists only in test builds, and it really is narrow:
    /// the guard's 403 branch has something to reject with.
    #[test]
    fn the_test_only_role_grants_status_and_nothing_else() {
        assert!(role_grants(ROLE_TEST_STATUS_ONLY, Capability::Status));
        for denied in [
            Capability::Stage,
            Capability::Apply,
            Capability::Restart,
            Capability::ManageAdmins,
        ] {
            assert!(
                !role_grants(ROLE_TEST_STATUS_ONLY, denied),
                "the narrow role must not grant {denied}"
            );
        }
        // It is not advertised as a role an operator may pick.
        assert!(!known_role_names().contains(&ROLE_TEST_STATUS_ONLY));
    }

    #[test]
    fn an_unknown_role_grants_nothing() {
        assert!(!role_is_known("contributor"));
        assert!(capabilities_for_role("contributor").is_none());
        assert!(!role_grants("contributor", Capability::Status));
        assert!(!role_grants("", Capability::Status));
        // Role names are matched exactly: near-misses are unknown roles, not
        // typos we quietly forgive.
        assert!(!role_is_known("Admin"));
        assert!(!role_grants("admin ", Capability::Status));
    }

    #[test]
    fn every_role_in_the_table_is_reachable_by_name() {
        for role in known_role_names() {
            assert!(role_is_known(role), "{role} listed but not resolvable");
            assert!(
                !capabilities_for_role(role).unwrap().is_empty(),
                "{role} grants nothing, which makes it a trap"
            );
        }
    }

    #[test]
    fn capability_names_are_distinct() {
        let names: Vec<&str> = [
            Capability::Status,
            Capability::Stage,
            Capability::Apply,
            Capability::Restart,
            Capability::ManageAdmins,
        ]
        .iter()
        .map(|c| c.as_str())
        .collect();
        let mut unique = names.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), names.len(), "capability names collide");
    }
}
