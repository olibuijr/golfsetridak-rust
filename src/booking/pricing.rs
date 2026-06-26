//! Hour-based slot pricing — ported from `src/lib/booking/pricing.ts`.
//!
//! A pricing rule covers an `[start_hour, end_hour)` window. When `start < end`
//! the window is a normal daytime span; when `start >= end` it **wraps past
//! midnight** (e.g. 22→8). Rules are tried in order and the first match wins,
//! exactly like the source. A per-user `fixed_price` overrides everything.

/// A single hourly pricing rule (the `pricing_rules` collection shape).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PricingRule {
    pub name: String,
    pub start_hour: i64,
    pub end_hour: i64,
    pub price: i64,
}

/// Does `hour` fall inside this rule's window (midnight-wrap aware)?
fn rule_covers(rule: &PricingRule, hour: i64) -> bool {
    if rule.start_hour < rule.end_hour {
        hour >= rule.start_hour && hour < rule.end_hour
    } else {
        // Wraps midnight: covers [start, 24) ∪ [0, end).
        hour >= rule.start_hour || hour < rule.end_hour
    }
}

/// Price for `hour` (0..=23) under `rules`; 0 if no rule matches. Mirrors
/// `getSlotPrice` — first matching rule wins, rule order is significant.
pub fn slot_price(hour: i64, rules: &[PricingRule]) -> i64 {
    for rule in rules {
        if rule_covers(rule, hour) {
            return rule.price;
        }
    }
    0
}

/// Effective price for `hour`: a user `fixed_price` overrides the rules,
/// otherwise the matched rule price. Mirrors `getEffectiveSlotPrice`.
pub fn effective_slot_price(hour: i64, rules: &[PricingRule], fixed_price: Option<i64>) -> i64 {
    match fixed_price {
        Some(p) => p,
        None => slot_price(hour, rules),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules() -> Vec<PricingRule> {
        // The source seed (`seed.ts`): evening, daytime, then an off-hours rule
        // that wraps midnight (22→8).
        vec![
            PricingRule {
                name: "Síðdegi/Kvöld".into(),
                start_hour: 16,
                end_hour: 22,
                price: 3500,
            },
            PricingRule {
                name: "Daytime".into(),
                start_hour: 8,
                end_hour: 16,
                price: 3500,
            },
            PricingRule {
                name: "Off-hours".into(),
                start_hour: 22,
                end_hour: 8,
                price: 2000,
            },
        ]
    }

    #[test]
    fn daytime_and_evening_match() {
        assert_eq!(slot_price(10, &rules()), 3500); // daytime
        assert_eq!(slot_price(18, &rules()), 3500); // evening
    }

    #[test]
    fn midnight_wrapping_off_hours_match() {
        assert_eq!(slot_price(23, &rules()), 2000); // after 22
        assert_eq!(slot_price(3, &rules()), 2000); // before 8
        assert_eq!(slot_price(0, &rules()), 2000); // exactly midnight
    }

    #[test]
    fn boundaries_are_half_open() {
        assert_eq!(slot_price(8, &rules()), 3500); // start inclusive (daytime)
        assert_eq!(slot_price(16, &rules()), 3500); // 16 belongs to evening
        assert_eq!(slot_price(22, &rules()), 2000); // 22 belongs to off-hours
    }

    #[test]
    fn no_rule_yields_zero() {
        assert_eq!(slot_price(10, &[]), 0);
    }

    #[test]
    fn fixed_price_overrides_rules() {
        assert_eq!(effective_slot_price(10, &rules(), Some(0)), 0);
        assert_eq!(effective_slot_price(10, &rules(), Some(1500)), 1500);
        assert_eq!(effective_slot_price(10, &rules(), None), 3500);
    }
}
