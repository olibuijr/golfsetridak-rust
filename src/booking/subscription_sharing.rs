//! Subscription member sharing — phone-based invites and role-based access control.
//!
//! Mirrors `src/lib/booking/subscription-sharing.ts` from the source app.
//! Members have roles (owner/member), statuses (active/invited/removed), and share
//! daily booking limits within a subscription.

// ---- Types ----

pub type SubscriptionMemberRole = String; // "owner" | "member"
pub type SubscriptionMemberStatus = String; // "active" | "invited" | "removed"

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserSubscriptionMember {
    pub id: String,
    pub user_subscription_id: String,
    pub user_id: Option<String>,
    pub role: SubscriptionMemberRole,
    pub status: SubscriptionMemberStatus,
    pub invited_phone: Option<String>,
    pub invited_at: i64,
    pub accepted_at: Option<i64>,
    pub removed_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SharedUsageSummary {
    pub used: i64,
    pub limit: i64,
    pub remaining: i64,
    pub exhausted: bool,
    pub label: String,
}

#[derive(Debug, Clone)]
pub struct ListMemberView {
    pub id: String,
    pub user_id: Option<String>,
    pub name: Option<String>,
    pub phone: Option<String>,
    pub invited_phone: Option<String>,
    pub role: String,
    pub status: String,
    pub invited_at: i64,
    pub accepted_at: Option<i64>,
}

// ---- Phone normalization ----

/// Normalize an Icelandic phone number to +354XXXXXXX format.
/// Accepts formats like:
/// - +354 XXXXXXX
/// - 00354 XXXXXXX
/// - 354 XXXXXXX
/// - XXXXXXX (7 digits, assumes +354)
pub fn normalize_subscription_invite_phone(input: &str) -> Result<String, String> {
    let raw = input.trim();
    if raw.is_empty() {
        return Err("Vantar símanúmer".into());
    }

    let mut digits = raw.replace(|c: char| !c.is_ascii_digit(), "");

    // Strip leading country codes
    if digits.starts_with("00") {
        digits = digits[2..].to_string();
    }
    if digits.starts_with("354") {
        digits = digits[3..].to_string();
    }

    // Must be exactly 7 digits
    if !digits.chars().all(|c| c.is_ascii_digit()) || digits.len() != 7 {
        return Err("Ógilt símanúmer — notaðu íslenskt 7 stafa númer".into());
    }

    Ok(format!("+354{}", digits))
}

/// Mask a phone number for display (show first and last 4 chars only).
pub fn mask_subscription_phone(phone: &str) -> Result<String, String> {
    let normalized = normalize_subscription_invite_phone(phone)?;
    Ok(format!(
        "{}****{}",
        &normalized[..4],
        &normalized[normalized.len() - 4..]
    ))
}

/// Summarize shared subscription daily usage across all active members.
pub fn summarize_shared_subscription_usage(
    daily_limit: i64,
    used_today: i64,
) -> SharedUsageSummary {
    let limit = daily_limit.max(0);
    let used = used_today.max(0);
    let remaining = (limit - used).max(0);
    SharedUsageSummary {
        used,
        limit,
        remaining,
        exhausted: remaining == 0,
        label: format!("{} af {} tímum notaðir í dag af hópnum", used, limit),
    }
}

/// Get a human-readable status label in Icelandic.
pub fn get_member_status_label(status: &str) -> &'static str {
    match status {
        "active" => "Virkur",
        "invited" => "Boð sent",
        _ => "Fjarlægður",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_phone_basic_7_digits() {
        let result = normalize_subscription_invite_phone("5123456");
        assert_eq!(result, Ok("+3545123456".into()));
    }

    #[test]
    fn normalize_phone_with_plus_354() {
        let result = normalize_subscription_invite_phone("+354 512 3456");
        assert_eq!(result, Ok("+3545123456".into()));
    }

    #[test]
    fn normalize_phone_with_00354() {
        let result = normalize_subscription_invite_phone("00354 512 3456");
        assert_eq!(result, Ok("+3545123456".into()));
    }

    #[test]
    fn normalize_phone_with_354_prefix() {
        let result = normalize_subscription_invite_phone("354 512 3456");
        assert_eq!(result, Ok("+3545123456".into()));
    }

    #[test]
    fn normalize_phone_rejects_empty() {
        let result = normalize_subscription_invite_phone("");
        assert!(result.is_err());
    }

    #[test]
    fn normalize_phone_rejects_wrong_length() {
        let result = normalize_subscription_invite_phone("512345"); // 6 digits
        assert!(result.is_err());
    }

    #[test]
    fn normalize_phone_rejects_non_numeric() {
        let result = normalize_subscription_invite_phone("5A23456");
        assert!(result.is_err());
    }

    #[test]
    fn mask_phone_correctly() {
        let result = mask_subscription_phone("5123456").unwrap();
        assert_eq!(result, "+354****3456");
    }

    #[test]
    fn summarize_usage_basic() {
        let summary = summarize_shared_subscription_usage(5, 2);
        assert_eq!(summary.used, 2);
        assert_eq!(summary.limit, 5);
        assert_eq!(summary.remaining, 3);
        assert!(!summary.exhausted);
    }

    #[test]
    fn summarize_usage_exhausted() {
        let summary = summarize_shared_subscription_usage(5, 5);
        assert_eq!(summary.remaining, 0);
        assert!(summary.exhausted);
    }

    #[test]
    fn summarize_usage_clamps_negatives() {
        let summary = summarize_shared_subscription_usage(-1, -3);
        assert_eq!(summary.limit, 0);
        assert_eq!(summary.used, 0);
        assert!(summary.exhausted);
    }

    #[test]
    fn get_status_label_active() {
        assert_eq!(get_member_status_label("active"), "Virkur");
    }

    #[test]
    fn get_status_label_invited() {
        assert_eq!(get_member_status_label("invited"), "Boð sent");
    }

    #[test]
    fn get_status_label_removed() {
        assert_eq!(get_member_status_label("removed"), "Fjarlægður");
    }
}
