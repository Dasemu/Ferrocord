//! Strict `MAJOR.MINOR.PATCH` versions (a tiny semver subset: releases are
//! tagged `v<Cargo version>`; pre-releases are never offered).

use std::cmp::Ordering;
use std::fmt;

/// A release version. Pre-release (`-rc.1`) and build (`+abc`) suffixes
/// are rejected at parse time: only plain releases are compared.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
}

impl Version {
    /// Parses `1.2.3` or `v1.2.3`. Leading zeros (`01.2.3`), missing parts,
    /// extra parts and any suffix are rejected, as semver does for
    /// releases.
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim();
        let s = s.strip_prefix('v').unwrap_or(s);
        let mut parts = s.split('.');
        let mut next = || -> Option<u64> {
            let p = parts.next()?;
            if p.is_empty()
                || p.len() > 18
                || !p.bytes().all(|b| b.is_ascii_digit())
                || (p.len() > 1 && p.starts_with('0'))
            {
                return None;
            }
            p.parse().ok()
        };
        let v = Self {
            major: next()?,
            minor: next()?,
            patch: next()?,
        };
        if parts.next().is_some() {
            return None;
        }
        Some(v)
    }

    /// The running build's version (`CARGO_PKG_VERSION`).
    pub fn current() -> Self {
        // Cargo enforces semver for `version`; a pre-release build would
        // fail to parse and is treated as 0.0.0 (every release is newer).
        Self::parse(env!("CARGO_PKG_VERSION")).unwrap_or(Self {
            major: 0,
            minor: 0,
            patch: 0,
        })
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.major, self.minor, self.patch).cmp(&(other.major, other.minor, other.patch))
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// Whether `candidate` (a tag such as `v0.18.0`) is a release newer than
/// `current`. Unparseable tags and pre-releases are never newer.
pub fn is_newer(current: Version, candidate: &str) -> bool {
    Version::parse(candidate).is_some_and(|c| c > current)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    #[test]
    fn parses_plain_and_prefixed_versions() {
        assert_eq!(
            v("1.2.3"),
            Version {
                major: 1,
                minor: 2,
                patch: 3
            }
        );
        assert_eq!(v("v0.17.1"), v("0.17.1"));
        assert_eq!(v(" 10.0.0\n"), v("10.0.0"));
        assert_eq!(v("0.17.1").to_string(), "0.17.1");
    }

    #[test]
    fn rejects_prereleases_and_garbage() {
        for bad in [
            "",
            "v",
            "1",
            "1.2",
            "1.2.3.4",
            "1.2.3-rc.1",
            "1.2.3+build",
            "01.2.3",
            "1.02.3",
            "1.2.x",
            "-1.2.3",
            "1..3",
            "vv1.2.3",
            "１.2.3",
            "99999999999999999999.0.0",
        ] {
            assert!(Version::parse(bad).is_none(), "{bad:?} must not parse");
        }
    }

    #[test]
    fn compares_numerically_not_lexically() {
        assert!(v("0.10.0") > v("0.9.9"));
        assert!(v("1.0.0") > v("0.99.99"));
        assert!(v("0.17.2") > v("0.17.1"));
        assert_eq!(v("0.17.1").cmp(&v("v0.17.1")), Ordering::Equal);
    }

    #[test]
    fn newer_only_for_strictly_greater_releases() {
        let cur = v("0.17.1");
        assert!(is_newer(cur, "v0.17.2"));
        assert!(is_newer(cur, "0.18.0"));
        assert!(!is_newer(cur, "v0.17.1"));
        assert!(!is_newer(cur, "v0.17.0"));
        assert!(!is_newer(cur, "v0.18.0-beta.1"));
        assert!(!is_newer(cur, "latest"));
    }

    #[test]
    fn current_matches_the_package_version() {
        assert_eq!(Version::current().to_string(), env!("CARGO_PKG_VERSION"));
    }
}
