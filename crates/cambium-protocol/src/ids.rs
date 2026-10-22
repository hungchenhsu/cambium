use std::{fmt, str::FromStr};

use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[derive(Debug, Clone, Copy, Eq, PartialEq, thiserror::Error)]
#[error("invalid identifier")]
pub struct InvalidId;

macro_rules! identifier {
    ($name:ident, $prefix:literal) => {
        #[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, schemars::JsonSchema)]
        pub struct $name(String);

        impl $name {
            pub fn new() -> Self {
                Self(format!(
                    "{}{}",
                    $prefix,
                    ulid::Ulid::new().to_string().to_ascii_lowercase()
                ))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl FromStr for $name {
            type Err = InvalidId;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                let suffix = value.strip_prefix($prefix).ok_or(InvalidId)?;
                if suffix.len() != 26
                    || !suffix
                        .bytes()
                        .all(|b| b"0123456789abcdefghjkmnpqrstvwxyz".contains(&b))
                    || suffix.as_bytes()[0] > b'7'
                {
                    return Err(InvalidId);
                }
                Ok(Self(value.to_owned()))
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(&self.0)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                String::deserialize(deserializer)?
                    .parse()
                    .map_err(serde::de::Error::custom)
            }
        }
    };
}

identifier!(TenantId, "ten_");
identifier!(IdentityId, "idn_");
identifier!(TokenId, "tok_");
identifier!(RepositoryId, "rep_");
identifier!(BranchId, "brn_");
identifier!(CompactionId, "cmp_");
identifier!(RequestId, "req_");

/// SHA-1 at the current protocol version. Never accepts abbreviated hashes.
#[derive(Clone, Eq, PartialEq, Ord, PartialOrd, Hash, schemars::JsonSchema)]
pub struct ObjectId(String);

impl ObjectId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for ObjectId {
    type Err = InvalidId;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.len() != 40
            || !value
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(InvalidId);
        }
        Ok(Self(value.to_owned()))
    }
}

impl fmt::Display for ObjectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl fmt::Debug for ObjectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Serialize for ObjectId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for ObjectId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifiers_validate_prefix_canonical_alphabet_and_ulid_overflow() {
        let id = TenantId::new();
        assert_eq!(id.as_str().parse::<TenantId>().unwrap(), id);
        assert!(id.as_str().parse::<RepositoryId>().is_err());
        for value in [
            "ten_80000000000000000000000000",
            "ten_0000000000000000000000000I",
            "ten_",
            "ten_0000000000000000000000000u",
        ] {
            assert!(value.parse::<TenantId>().is_err(), "{value}");
        }
        assert!("a".repeat(40).parse::<ObjectId>().is_ok());
        assert!("A".repeat(40).parse::<ObjectId>().is_err());
        assert!("a".repeat(39).parse::<ObjectId>().is_err());
    }
}
