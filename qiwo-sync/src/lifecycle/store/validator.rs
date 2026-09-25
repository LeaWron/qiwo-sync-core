use super::{TransportProfile, capability_error, require_strong_etag};
use anyhow::Result;

/// A token is bound to its checked transport contract; callers cannot construct it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Validator {
    pub(super) profile: TransportProfile,
    pub(super) value: String,
}
impl Validator {
    pub(super) fn parse(profile: TransportProfile, value: &str) -> Result<Self> {
        match profile {
            TransportProfile::Standard => require_strong_etag(value)?,
            TransportProfile::OpaqueMove => {
                if value.is_empty()
                    || value.len() > 256
                    || !value
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
                {
                    return Err(capability_error("服务器返回了不允许的兼容条件标记"));
                }
            }
        }
        Ok(Self {
            profile,
            value: value.into(),
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn opaque_tokens_never_accept_weak_or_wildcard_conditions() {
        for token in [
            "",
            "*",
            "W/abc",
            "W/\"abc\"",
            "\"abc\"",
            "a,b",
            "a b",
            "a\r\nb",
            "中文",
        ] {
            assert!(
                Validator::parse(TransportProfile::OpaqueMove, token).is_err(),
                "{token}"
            );
        }
        assert!(Validator::parse(TransportProfile::OpaqueMove, &"a".repeat(257)).is_err());
        assert_eq!(
            Validator::parse(TransportProfile::OpaqueMove, "a-b_C123")
                .unwrap()
                .value,
            "a-b_C123"
        );
        assert!(Validator::parse(TransportProfile::Standard, "unquoted").is_err());
        assert!(Validator::parse(TransportProfile::Standard, "\"standard\"").is_ok());
    }
}
