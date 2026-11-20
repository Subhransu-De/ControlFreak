use serde::{Deserialize, Serialize};

/// The version of the [`BuildInfo`] object. Raise it when a field changes meaning or is removed.
pub const BUILD_INFO_SCHEMA_VERSION: u32 = 1;

/// The revision reported when the build had neither Git metadata nor a revision from CI.
pub const UNKNOWN_REVISION: &str = "unknown";

/// What the running executable says about the source it was built from. Every field is fixed at
/// compile time and carries no wall-clock time, path, user name or host name, so two builds of the
/// same source report the same object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildInfo {
    pub schema_version: u32,
    /// The package version, as in `serverInfo.version`.
    pub version: String,
    /// The full Git commit hash, or `unknown`.
    pub revision: String,
    /// Whether tracked files differed from `revision`. `None` when the build had no Git metadata
    /// and so could not tell.
    pub dirty: Option<bool>,
    /// The Rust target triple, such as `x86_64-pc-windows-msvc`.
    pub target: String,
}

impl BuildInfo {
    #[must_use]
    pub fn new(
        version: impl Into<String>,
        revision: impl Into<String>,
        dirty: Option<bool>,
        target: impl Into<String>,
    ) -> Self {
        Self {
            schema_version: BUILD_INFO_SCHEMA_VERSION,
            version: version.into(),
            revision: revision.into(),
            dirty,
            target: target.into(),
        }
    }

    /// Whether every field has its documented format: a version of ASCII letters, digits, `.`,
    /// `+` and `-`; a 40 or 64 digit lowercase hex revision or `unknown`; a target of lowercase
    /// ASCII letters, digits, `_` and `-`. None of these can hold a path, a user or host name with
    /// a separator, or a timestamp with a colon.
    #[must_use]
    pub fn is_well_formed(&self) -> bool {
        let version = !self.version.is_empty()
            && self
                .version
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'+' | b'-'));
        let target = !self.target.is_empty()
            && self.target.bytes().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-')
            });
        self.schema_version == BUILD_INFO_SCHEMA_VERSION
            && version
            && (self.revision == UNKNOWN_REVISION || is_commit_hash(&self.revision))
            && target
    }
}

/// A full SHA-1 or SHA-256 Git object name in lowercase hex.
#[must_use]
pub fn is_commit_hash(value: &str) -> bool {
    matches!(value.len(), 40 | 64)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[cfg(test)]
mod tests {
    use super::*;

    const COMMIT: &str = "aa56f6300000000000000000000000000000beef";

    #[test]
    fn a_git_build_and_the_fallback_are_well_formed() {
        assert!(
            BuildInfo::new("0.2.3", COMMIT, Some(false), "x86_64-pc-windows-msvc").is_well_formed()
        );
        assert!(BuildInfo::new("0.3.0-rc.1", UNKNOWN_REVISION, None, "unknown").is_well_formed());
        assert!(
            BuildInfo::new(
                "1.0.0",
                "a".repeat(64),
                Some(true),
                "aarch64-pc-windows-msvc"
            )
            .is_well_formed()
        );
    }

    #[test]
    fn paths_names_and_timestamps_are_not_well_formed() {
        let target = "x86_64-pc-windows-msvc";
        for revision in [
            "",
            "abc123",
            &COMMIT.to_uppercase(),
            "C:\\Users\\someone\\src",
            "/home/someone/src",
            "someone@host",
            "2026-11-20T10:00:00Z",
        ] {
            assert!(
                !BuildInfo::new("0.2.3", revision, None, target).is_well_formed(),
                "{revision}"
            );
        }
        for version in ["", "0.2.3 built by someone", "0.2.3/host", "0.2.3:12"] {
            assert!(
                !BuildInfo::new(version, COMMIT, None, target).is_well_formed(),
                "{version}"
            );
        }
        for target in ["", "C:\\toolchains", "x86_64 pc", "X86_64-PC-WINDOWS-MSVC"] {
            assert!(
                !BuildInfo::new("0.2.3", COMMIT, None, target).is_well_formed(),
                "{target}"
            );
        }
        let mut info = BuildInfo::new("0.2.3", COMMIT, None, target);
        info.schema_version = 0;
        assert!(!info.is_well_formed());
    }

    #[test]
    fn the_object_serializes_with_a_null_dirty_marker_when_unknown() {
        let value =
            serde_json::to_value(BuildInfo::new("0.2.3", UNKNOWN_REVISION, None, "unknown"))
                .unwrap();
        assert_eq!(
            value,
            serde_json::json!({
                "schema_version": 1,
                "version": "0.2.3",
                "revision": "unknown",
                "dirty": null,
                "target": "unknown",
            })
        );
    }
}
