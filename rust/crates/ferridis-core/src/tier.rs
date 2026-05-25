//! Execution tiers — how a capability call is actually executed.

use serde::{Deserialize, Serialize};

use crate::error::FerridisError;

/// The tier at which a capability call is executed.
///
/// Default precedence (highest to lowest): [`Tier::Native`], [`Tier::Browser`],
/// [`Tier::Vision`]. The runtime picks the highest-precedence tier the
/// connection supports, unless the user has set an override.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Tier {
    /// Native API call (HTTP, structured request/response).
    Native,
    /// Browser session (drive a logged-in browser).
    Browser,
    /// Computer-use vision (drive the UI visually). Last resort.
    Vision,
}

impl Tier {
    /// Lower number = higher precedence.
    pub fn precedence(self) -> u8 {
        match self {
            Tier::Native => 0,
            Tier::Browser => 1,
            Tier::Vision => 2,
        }
    }
}

/// A non-empty set of supported tiers.
///
/// A capability must support at least one tier. The [`Tiers`] type
/// enforces this at construction time — there is no way to construct
/// an empty `Tiers` value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "Vec<Tier>", into = "Vec<Tier>")]
pub struct Tiers {
    head: Tier,
    rest: Vec<Tier>,
}

impl Tiers {
    /// Build a `Tiers` from a head and a possibly-empty tail.
    pub fn new(head: Tier, rest: Vec<Tier>) -> Self {
        Self { head, rest }
    }

    /// Build a `Tiers` from a vector. Returns an error if the vector is empty.
    pub fn from_vec(mut v: Vec<Tier>) -> Result<Self, FerridisError> {
        if v.is_empty() {
            return Err(FerridisError::MissingField("tiers"));
        }
        let head = v.remove(0);
        Ok(Self { head, rest: v })
    }

    /// Iterate over all supported tiers.
    pub fn iter(&self) -> impl Iterator<Item = Tier> + '_ {
        std::iter::once(self.head).chain(self.rest.iter().copied())
    }

    /// Return the highest-precedence tier in this set.
    pub fn preferred(&self) -> Tier {
        self.iter()
            .min_by_key(|t| t.precedence())
            .expect("Tiers is non-empty by construction")
    }

    /// Whether this set contains a given tier.
    pub fn contains(&self, t: Tier) -> bool {
        self.iter().any(|x| x == t)
    }
}

impl TryFrom<Vec<Tier>> for Tiers {
    type Error = FerridisError;
    fn try_from(v: Vec<Tier>) -> Result<Self, Self::Error> {
        Self::from_vec(v)
    }
}

impl From<Tiers> for Vec<Tier> {
    fn from(t: Tiers) -> Self {
        t.iter().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tiers_rejects_empty() {
        assert!(Tiers::from_vec(vec![]).is_err());
    }

    #[test]
    fn preferred_picks_highest_precedence() {
        let t = Tiers::from_vec(vec![Tier::Browser, Tier::Native, Tier::Vision]).unwrap();
        assert_eq!(t.preferred(), Tier::Native);
    }

    #[test]
    fn contains_works() {
        let t = Tiers::from_vec(vec![Tier::Browser, Tier::Vision]).unwrap();
        assert!(t.contains(Tier::Browser));
        assert!(!t.contains(Tier::Native));
    }
}
