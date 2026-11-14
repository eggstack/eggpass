//! The x.ai usage collector.
//!
//! x.ai's CLI proxy exposes the consumer subscription pool at
//! `GET /v1/billing?format=credits`, with the same OAuth token `eggpass`
//! already stores. The endpoint is not a separately guaranteed public API —
//! it is part of an internal CLI proxy — so everything here treats every
//! field as optional and reports `unavailable` rather than a wrong number.
//!
//! Two facts shape the parsing:
//!
//! * The response is **proto3 JSON**, which omits zero-valued scalars. A
//!   missing `creditUsagePercent` therefore means "the provider did not say",
//!   not "zero per cent used".
//! * `format=credits` is **not optional**. Without it the endpoint answers
//!   with the deprecated shape (`monthlyLimit`, `used`, `history[]`), which
//!   carries no percentage at all. That shape is deliberately not parsed: a
//!   number derived from a deprecated shape would be a guess.

use serde_json::Value;

use crate::provider::ProviderRuntime;
use crate::provider::usage::{
    Credits, ProductUsage, ProviderUsage, QuotaState, QuotaWindow, UsageError, UsageReport,
    WindowLabel,
};
use crate::timeutil;
use crate::upstream::{Method, UpstreamClient, UpstreamError, send_with_auth};

/// The origin the billing endpoint lives on.
///
/// The billing endpoint is **not** the configured upstream: it lives on the
/// CLI proxy whatever the configuration says, because it reports the
/// consumer subscription rather than the API tier.
pub const BILLING_BASE_URL: &str = "https://cli-chat-proxy.grok.com";

/// The path of the billing endpoint, with the format that selects the current
/// shape.
pub const BILLING_PATH: &str = "/v1/billing?format=credits";

/// The full billing URL.
pub const BILLING_URL: &str = "https://cli-chat-proxy.grok.com/v1/billing?format=credits";

/// The path of the settings endpoint, which carries the plan name.
pub const SETTINGS_PATH: &str = "/v1/settings";

/// The full settings URL.
pub const SETTINGS_URL: &str = "https://cli-chat-proxy.grok.com/v1/settings";

/// The token header the CLI proxy's own auth middleware names.
pub const TOKEN_AUTH_HEADER: &str = "X-XAI-Token-Auth";

/// The value the CLI proxy expects in the token header.
pub const TOKEN_AUTH_VALUE: &str = "xai-grok-cli";

/// The note that says which pool the number belongs to.
///
/// On the default `api` upstream, `eggpass`'s inference is metered against
/// the standard xAI API tier and does **not** draw from the consumer
/// subscription the billing endpoint reports. A user who reads "40 % used" and
/// then watches their API bill has been misled by the tool, so the caption
/// says which pool it is.
pub fn caption(upstream: crate::config::Upstream) -> &'static str {
    match upstream {
        crate::config::Upstream::Build => "This is the pool `eggpass` draws from.",
        crate::config::Upstream::Api => {
            "This is your Grok subscription. `eggpass` requests are billed separately against the xAI API tier."
        }
    }
}

/// Ask x.ai what it can say about the allowance.
pub fn collect(runtime: &ProviderRuntime) -> ProviderUsage {
    // The request goes to the URL the docs above name. The assertion catches
    // a path that drifted away from it.
    debug_assert_eq!(format!("{BILLING_BASE_URL}{BILLING_PATH}"), BILLING_URL);
    debug_assert_eq!(format!("{BILLING_BASE_URL}{SETTINGS_PATH}"), SETTINGS_URL);

    let client =
        UpstreamClient::for_endpoint(BILLING_BASE_URL, &headers(runtime), runtime.timeouts);

    let mut response = match send_with_auth(
        &client,
        &runtime.tokens,
        Method::Get,
        BILLING_PATH,
        &[],
        None,
    ) {
        Ok(response) => response,
        Err(err) => {
            return ProviderUsage::Failed {
                reason: describe(&err),
            };
        }
    };

    match response.status {
        200 => {}
        401 => {
            return ProviderUsage::Failed {
                reason: "the provider rejected the token".to_string(),
            };
        }
        404 => {
            return ProviderUsage::Failed {
                reason: "the endpoint is not available (HTTP 404)".to_string(),
            };
        }
        429 => {
            return ProviderUsage::Failed {
                reason: "the provider is rate-limiting this account".to_string(),
            };
        }
        status => {
            return ProviderUsage::Failed {
                reason: format!("the provider answered HTTP {status}"),
            };
        }
    }

    let body = match response.read_to_vec() {
        Ok(body) => body,
        Err(err) => {
            return ProviderUsage::Failed {
                reason: format!("could not read the provider's answer ({err})"),
            };
        }
    };

    let mut report = match parse_billing(&body) {
        Ok(report) => report,
        Err(err) => {
            crate::logging::warn(&format!("billing response: {err}"));
            return ProviderUsage::Failed {
                reason: err.to_string(),
            };
        }
    };

    // The plan name is enrichment, not the feature. A failure here omits the
    // label; the usage still prints.
    report.plan = fetch_plan(&client, runtime);

    if report.says_nothing() {
        return ProviderUsage::Unavailable {
            reason: "the provider reported no usage information".to_string(),
            source: None,
            hint: None,
            signals: Vec::new(),
        };
    }

    ProviderUsage::Reported { report }
}

/// The headers the billing request carries.
///
/// The official client's billing request sends the bearer token, the token
/// header, `Accept`, the client version and the user id. It does **not** send
/// `x-grok-client-identifier` or `x-authenticateresponse`: those belong to
/// proxy-routed inference, not to billing, and `eggpass`'s own provider spec
/// attaches the identifier to inference requests.
fn headers(runtime: &ProviderRuntime) -> Vec<(&'static str, String)> {
    let mut headers = vec![
        (TOKEN_AUTH_HEADER, TOKEN_AUTH_VALUE.to_string()),
        ("Accept", "application/json".to_string()),
        (
            "x-grok-client-version",
            crate::provider::xai::GROK_BUILD_CLIENT_VERSION.to_string(),
        ),
    ];

    // The official client sends the user id; it is not required here, but it
    // costs nothing and the proxy is happier with it.
    if let Some(user_id) = runtime
        .tokens
        .current()
        .ok()
        .flatten()
        .and_then(|auth| auth.user_id)
    {
        headers.push(("x-userid", user_id));
    }

    headers
}

/// The plan name, from the settings endpoint.
///
/// Best effort: any failure means no plan name, not a failed report.
fn fetch_plan(client: &UpstreamClient, runtime: &ProviderRuntime) -> Option<String> {
    let mut response = send_with_auth(
        client,
        &runtime.tokens,
        Method::Get,
        SETTINGS_PATH,
        &[],
        None,
    )
    .ok()?;
    if response.status != 200 {
        return None;
    }
    let body = response.read_to_vec().ok()?;
    parse_settings(&body)
}

/// Parse the billing body into a report.
///
/// The function is pure, so every rule below is tested without a network.
pub fn parse_billing(body: &[u8]) -> Result<UsageReport, UsageError> {
    let value: Value = serde_json::from_slice(body).map_err(|err| UsageError::Malformed {
        field: "the response body".to_string(),
        message: format!("it is not JSON ({err})"),
    })?;

    let mut report = UsageReport {
        provider: crate::provider::ProviderId::Xai,
        plan: None,
        windows: Vec::new(),
        breakdown: Vec::new(),
        credits: None,
        state: None,
    };

    // `config` may be null, or absent entirely. Both mean the provider said
    // nothing, and both are legal.
    let Some(config) = value.get("config").filter(|config| config.is_object()) else {
        return Ok(report);
    };

    let used_percent = match read_percent(
        config.get("creditUsagePercent"),
        "config.creditUsagePercent",
    ) {
        Ok(value) => value,
        Err(err) => {
            // The field path is logged; the body never is.
            crate::logging::warn(&format!("billing response: {err}"));
            None
        }
    };

    let period = config
        .get("currentPeriod")
        .filter(|period| period.is_object());
    let label = match period
        .and_then(|period| period.get("type"))
        .and_then(Value::as_str)
    {
        Some(raw) => window_label(raw),
        None => WindowLabel::Other(String::new()),
    };

    // The reset time comes from the current period, and falls back to the
    // billing period when there is no current period.
    let resets_at = period
        .and_then(|period| period.get("end"))
        .and_then(Value::as_str)
        .and_then(timeutil::parse_rfc3339_to_unix)
        .or_else(|| {
            config
                .get("billingPeriodEnd")
                .and_then(Value::as_str)
                .and_then(timeutil::parse_rfc3339_to_unix)
        });

    report.windows.push(QuotaWindow {
        label,
        used_percent,
        resets_at,
        duration_secs: None,
    });

    report.breakdown = read_products(config.get("productUsage"));
    report.credits = read_credits(config);
    report.state = read_state(config);

    Ok(report)
}

/// Read the plan name from the settings body.
///
/// The field is read at the top level, and one level down, because the
/// settings endpoint nests its answers differently from the billing one.
pub fn parse_settings(body: &[u8]) -> Option<String> {
    let value: Value = serde_json::from_slice(body).ok()?;
    let tier = find_tier(&value)?;
    let tier = tier.trim();
    if tier.is_empty() {
        return None;
    }
    Some(tier.to_string())
}

/// Find `subscription_tier_display` in the settings body.
fn find_tier(value: &Value) -> Option<&str> {
    const FIELD: &str = "subscription_tier_display";

    if let Some(text) = value.get(FIELD).and_then(Value::as_str) {
        return Some(text);
    }

    let object = value.as_object()?;
    for nested in object.values() {
        if let Some(text) = nested.get(FIELD).and_then(Value::as_str) {
            return Some(text);
        }
    }

    None
}

/// Read a percentage, keeping "not said" apart from zero.
///
/// A value outside 0–100 is malformed rather than clamped: clamping would
/// invent a number the provider did not give.
fn read_percent(value: Option<&Value>, field: &str) -> Result<Option<f64>, UsageError> {
    let Some(value) = value else {
        return Ok(None);
    };

    // An explicit null is the same as an omission: the provider did not say.
    if value.is_null() {
        return Ok(None);
    }

    let Some(number) = value.as_f64() else {
        return Err(UsageError::Malformed {
            field: field.to_string(),
            message: "is not a number".to_string(),
        });
    };

    if !(0.0..=100.0).contains(&number) {
        return Err(UsageError::Malformed {
            field: field.to_string(),
            message: "is outside the range 0 to 100".to_string(),
        });
    }

    // Proto3 omits a zero-valued scalar, so a zero here is indistinguishable
    // from an omission. Both mean "the provider did not say", and neither may
    // be shown as "0 % used".
    if number == 0.0 {
        return Ok(None);
    }

    Ok(Some(number))
}

/// Read the per-product breakdown.
fn read_products(value: Option<&Value>) -> Vec<ProductUsage> {
    const FIELD: &str = "config.productUsage[].usagePercent";

    let Some(entries) = value.and_then(Value::as_array) else {
        return Vec::new();
    };

    entries
        .iter()
        .filter_map(|entry| {
            let product = entry.get("product").and_then(Value::as_str)?;
            let used_percent = match read_percent(entry.get("usagePercent"), FIELD) {
                Ok(value) => value,
                Err(err) => {
                    crate::logging::warn(&format!("billing response: {err}"));
                    None
                }
            };
            Some(ProductUsage {
                product: product.to_string(),
                used_percent,
            })
        })
        .collect()
}

/// Read the purchased credit.
///
/// The proto3 rule applies twice, in opposite directions:
///
/// * `prepaidBalance` **absent** means unknown.
/// * `prepaidBalance` **present** with no `val` means **zero**, because `val`
///   is omitted when it is zero.
fn read_credits(config: &Value) -> Option<Credits> {
    let prepaid_cents = read_cents(config.get("prepaidBalance"));
    let on_demand_used_cents = read_cents(config.get("onDemandUsed"));
    let on_demand_cap_cents = read_cents(config.get("onDemandCap"));

    if prepaid_cents.is_none() && on_demand_used_cents.is_none() && on_demand_cap_cents.is_none() {
        return None;
    }

    Some(Credits {
        prepaid_cents,
        on_demand_used_cents,
        on_demand_cap_cents,
    })
}

/// Read one credit amount, in cents.
fn read_cents(value: Option<&Value>) -> Option<i64> {
    let value = value?;
    if !value.is_object() {
        return None;
    }

    match value.get("val") {
        // Present, but without a value: the amount is zero. An explicit null
        // says the same thing.
        None => Some(0),
        Some(number) if number.is_null() => Some(0),
        Some(number) => {
            if let Some(cents) = number.as_i64() {
                return Some(cents);
            }
            if let Some(cents) = number.as_f64() {
                return Some(cents.round() as i64);
            }
            None
        }
    }
}

/// Read the provider's own statement about the allowance.
///
/// Only an explicit field counts. A percentage of 100 % is not a state: it is
/// a percentage that happens to be 100, and inferring "limit reached" from it
/// would be an inference.
fn read_state(config: &Value) -> Option<QuotaState> {
    match config.get("isUnifiedBillingUser") {
        Some(Value::Bool(false)) => Some(QuotaState::NotEligible),
        _ => None,
    }
}

/// The window label for a period type.
///
/// The type is a proto enum name, so it is matched on the suffix: a
/// differently-prefixed `SOMETHING_WEEKLY` is still weekly, and an
/// unrecognised type is kept verbatim rather than guessed at.
fn window_label(raw: &str) -> WindowLabel {
    let suffix = raw.rsplit('_').next().unwrap_or(raw);
    match suffix.to_ascii_uppercase().as_str() {
        "WEEKLY" => WindowLabel::Weekly,
        "MONTHLY" => WindowLabel::Monthly,
        "DAILY" => WindowLabel::Daily,
        _ => WindowLabel::Other(raw.to_string()),
    }
}

/// Describe an upstream failure in one line.
fn describe(err: &UpstreamError) -> String {
    match err {
        UpstreamError::Network(message) => {
            format!("the provider could not be reached ({message})")
        }
        UpstreamError::Protocol(message) => format!("the provider answered badly ({message})"),
        UpstreamError::Token(err) => err.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::ProviderId;

    /// The payload captured first-hand on 2026-11-13.
    const VERIFIED: &str = r#"{"config":{
      "currentPeriod":{"type":"USAGE_PERIOD_TYPE_WEEKLY",
                       "start":"2026-11-13T03:07:08Z","end":"2026-11-20T03:07:08Z"},
      "creditUsagePercent":1.0,
      "productUsage":[{"product":"Cursor","usagePercent":1.0}],
      "isUnifiedBillingUser":true,
      "prepaidBalance":{"val":0},
      "topUpMethod":"TOP_UP_METHOD_SAVED_PAYMENT_METHOD",
      "billingPeriodStart":"2026-11-13T03:07:08Z",
      "billingPeriodEnd":"2026-11-20T03:07:08Z"}}"#;

    #[test]
    fn the_verified_payload_is_parsed() {
        let report = parse_billing(VERIFIED.as_bytes()).unwrap();

        assert_eq!(report.provider, ProviderId::Xai);
        assert_eq!(report.windows.len(), 1);
        assert_eq!(report.windows[0].used_percent, Some(1.0));
        assert!(matches!(report.windows[0].label, WindowLabel::Weekly));
        // The literal, not the parser's own answer to the same question:
        // `2026-11-20T03:07:08Z` is 1795144028 seconds after the epoch.
        assert_eq!(report.windows[0].resets_at, Some(1_795_144_028));

        // The prepaid balance was zero, and zero is still zero.
        let credits = report.credits.expect("the payload carries a balance");
        assert_eq!(credits.prepaid_cents, Some(0));

        // A unified billing user has no state to report.
        assert!(report.state.is_none());
    }

    #[test]
    fn a_missing_percentage_is_unknown_not_zero() {
        let body = br#"{"config":{"currentPeriod":{"type":"USAGE_PERIOD_TYPE_WEEKLY"}}}"#;
        let report = parse_billing(body).unwrap();
        assert_eq!(report.windows[0].used_percent, None);
    }

    #[test]
    fn a_zero_percentage_is_the_same_as_a_missing_one() {
        // Proto3 omits a zero-valued scalar, so the wire never carries one.
        // If it ever does, it still does not mean "the provider said zero".
        let body = br#"{"config":{"creditUsagePercent":0.0}}"#;
        let report = parse_billing(body).unwrap();
        assert_eq!(report.windows[0].used_percent, None);
    }

    #[test]
    fn the_period_type_is_read_from_the_enum_name() {
        let body = br#"{"config":{"currentPeriod":{"type":"USAGE_PERIOD_TYPE_WEEKLY"}}}"#;
        let report = parse_billing(body).unwrap();
        assert!(matches!(report.windows[0].label, WindowLabel::Weekly));
    }

    #[test]
    fn an_unknown_period_type_is_kept_verbatim() {
        let body = br#"{"config":{"currentPeriod":{"type":"USAGE_PERIOD_TYPE_FORTNIGHTLY"}}}"#;
        let report = parse_billing(body).unwrap();
        assert!(matches!(
            report.windows[0].label,
            WindowLabel::Other(ref raw) if raw == "USAGE_PERIOD_TYPE_FORTNIGHTLY"
        ));
    }

    #[test]
    fn the_period_type_matches_on_a_suffix() {
        let body = br#"{"config":{"currentPeriod":{"type":"USAGE_PERIOD_TYPE_MONTHLY"}}}"#;
        let report = parse_billing(body).unwrap();
        assert!(matches!(report.windows[0].label, WindowLabel::Monthly));

        let body = br#"{"config":{"currentPeriod":{"type":"SOMETHING_WEEKLY"}}}"#;
        let report = parse_billing(body).unwrap();
        assert!(matches!(report.windows[0].label, WindowLabel::Weekly));
    }

    #[test]
    fn a_null_config_is_unavailable() {
        let report = parse_billing(br#"{"config":null}"#).unwrap();
        assert!(report.windows.is_empty());
        assert!(report.says_nothing());
    }

    #[test]
    fn an_absent_config_is_unavailable() {
        let report = parse_billing(br#"{}"#).unwrap();
        assert!(report.windows.is_empty());
        assert!(report.says_nothing());
    }

    #[test]
    fn an_empty_prepaid_balance_is_zero() {
        let body = br#"{"config":{"prepaidBalance":{}}}"#;
        let report = parse_billing(body).unwrap();
        let credits = report.credits.expect("the balance is there");
        assert_eq!(credits.prepaid_cents, Some(0));
    }

    #[test]
    fn an_absent_prepaid_balance_is_unknown() {
        let body = br#"{"config":{"creditUsagePercent":12.5}}"#;
        let report = parse_billing(body).unwrap();
        assert!(report.credits.is_none());
    }

    #[test]
    fn a_null_value_is_the_same_as_an_absent_one() {
        // Proto3 omits a zero-valued scalar, so a null never turns up. If one
        // does, it says the same thing as the omission.
        let body = br#"{"config":{"creditUsagePercent":null,"prepaidBalance":{"val":null}}}"#;
        let report = parse_billing(body).unwrap();
        assert_eq!(report.windows[0].used_percent, None);
        let credits = report.credits.expect("the balance is there");
        assert_eq!(credits.prepaid_cents, Some(0));
    }

    #[test]
    fn the_on_demand_amounts_are_read_as_cents() {
        let body = br#"{"config":{"onDemandUsed":{"val":123},"onDemandCap":{"val":5000}}}"#;
        let report = parse_billing(body).unwrap();
        let credits = report.credits.expect("the amounts are there");
        assert_eq!(credits.on_demand_used_cents, Some(123));
        assert_eq!(credits.on_demand_cap_cents, Some(5_000));
        assert_eq!(credits.prepaid_cents, None);
    }

    #[test]
    fn an_out_of_range_percentage_is_rejected() {
        let body = br#"{"config":{"creditUsagePercent":100.5}}"#;
        let report = parse_billing(body).unwrap();
        assert_eq!(report.windows[0].used_percent, None);
    }

    #[test]
    fn a_percentage_out_of_range_names_the_field() {
        // The parser drops the number; the diagnostic says which field it was.
        let body = br#"{"config":{"creditUsagePercent":100.5}}"#;
        let report = parse_billing(body).unwrap();
        assert_eq!(report.windows[0].used_percent, None);

        let value = serde_json::json!(100.5);
        let err = read_percent(Some(&value), "config.creditUsagePercent").unwrap_err();
        let text = err.to_string();
        assert!(text.contains("config.creditUsagePercent"), "{text}");
        assert!(text.contains("outside the range"), "{text}");
    }

    #[test]
    fn a_percentage_that_is_not_a_number_names_the_field() {
        let value = serde_json::json!("forty");
        let err = read_percent(Some(&value), "config.creditUsagePercent").unwrap_err();
        let text = err.to_string();
        assert!(text.contains("config.creditUsagePercent"), "{text}");
        assert!(text.contains("is not a number"), "{text}");
    }

    #[test]
    fn a_product_without_a_percentage_is_kept() {
        let body = br#"{"config":{"productUsage":[{"product":"GrokChat"}]}}"#;
        let report = parse_billing(body).unwrap();
        assert_eq!(report.breakdown.len(), 1);
        assert_eq!(report.breakdown[0].product, "GrokChat");
        assert_eq!(report.breakdown[0].used_percent, None);
    }

    #[test]
    fn a_reset_time_is_parsed() {
        let body = br#"{"config":{"currentPeriod":{"end":"2026-11-20T03:07:08Z"}}}"#;
        let report = parse_billing(body).unwrap();
        assert_eq!(report.windows[0].resets_at, Some(1_795_144_028));
    }

    #[test]
    fn an_offset_timestamp_is_parsed() {
        // The microseconds and the explicit offset do not move the second.
        let body = br#"{"config":{"currentPeriod":{"end":"2026-11-20T03:07:08.930537+00:00"}}}"#;
        let report = parse_billing(body).unwrap();
        assert_eq!(report.windows[0].resets_at, Some(1_795_144_028));
    }

    #[test]
    fn the_billing_period_is_the_fallback_reset_time() {
        let body = br#"{"config":{"billingPeriodEnd":"2026-11-20T03:07:08Z"}}"#;
        let report = parse_billing(body).unwrap();
        assert_eq!(report.windows[0].resets_at, Some(1_795_144_028));
    }

    #[test]
    fn a_non_json_body_is_an_error() {
        let err = parse_billing(b"<html>").unwrap_err();
        let text = err.to_string();
        assert!(text.contains("malformed"), "{text}");
        assert!(!text.contains("<html>"), "{text}");
    }

    #[test]
    fn an_ineligible_account_is_a_state() {
        let body = br#"{"config":{"isUnifiedBillingUser":false,"creditUsagePercent":40}}"#;
        let report = parse_billing(body).unwrap();
        assert_eq!(report.state, Some(QuotaState::NotEligible));
    }

    #[test]
    fn the_plan_name_is_read_from_the_settings_body() {
        let body = br#"{"subscription_tier_display":"SuperGrok"}"#;
        assert_eq!(parse_settings(body).as_deref(), Some("SuperGrok"));

        let body = br#"{"config":{"subscription_tier_display":"SuperGrok Heavy"}}"#;
        assert_eq!(parse_settings(body).as_deref(), Some("SuperGrok Heavy"));

        assert_eq!(parse_settings(br#"{}"#), None);
        assert_eq!(parse_settings(b"not json"), None);
    }

    #[test]
    fn the_caption_names_the_pool() {
        let api = caption(crate::config::Upstream::Api);
        assert!(
            api.contains("billed separately against the xAI API tier"),
            "{api}"
        );

        let build = caption(crate::config::Upstream::Build);
        assert!(build.contains("draws from"), "{build}");
    }
}
