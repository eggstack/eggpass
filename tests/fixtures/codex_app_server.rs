use std::io::{self, BufRead, Write};
use std::thread;
use std::time::Duration;

fn main() {
    let mode = std::env::var("EGGPASS_CODEX_TEST_MODE").unwrap_or_default();
    let stdin = io::stdin();
    let mut stdout = io::stdout().lock();
    let mut stage = 0_u8;

    for line in stdin.lock().lines() {
        let Ok(line) = line else { return };
        let method = if line.contains("\"method\":\"initialize\"") {
            Some("initialize")
        } else if line.contains("\"method\":\"initialized\"") {
            Some("initialized")
        } else if line.contains("\"method\":\"account/read\"") {
            Some("account/read")
        } else if line.contains("\"method\":\"account/rateLimits/read\"") {
            Some("account/rateLimits/read")
        } else {
            None
        };
        let Some(method) = method else { return };

        if mode == "early_exit" {
            return;
        }
        if mode == "stall_initialize" && method == "initialize" {
            thread::sleep(Duration::from_secs(30));
            return;
        }
        if mode == "stall_account" && method == "account/read" {
            thread::sleep(Duration::from_secs(30));
            return;
        }
        if mode == "stall_rate_limits" && method == "account/rateLimits/read" {
            thread::sleep(Duration::from_secs(30));
            return;
        }

        match method {
            "initialize" => {
                if stage != 0
                    || !line.contains("\"name\":\"eggpass\"")
                    || !line.contains("\"version\":")
                    || line.contains("\"experimentalApi\":true")
                {
                    send_error(&mut stdout, id(&line), "bad initialize request");
                    return;
                }
                stage = 1;
                if mode == "malformed" {
                    let _ = writeln!(stdout, "not-json");
                    let _ = stdout.flush();
                    return;
                }
                if mode == "oversized" {
                    let _ = writeln!(stdout, "{}", "x".repeat(300 * 1024));
                    let _ = stdout.flush();
                    return;
                }
                if mode == "stdout_total" {
                    let payload = "x".repeat(220 * 1024);
                    for _ in 0..5 {
                        let _ = writeln!(
                            stdout,
                            "{{\"method\":\"noise\",\"params\":{{\"data\":\"{payload}\"}}}}"
                        );
                    }
                    let _ = stdout.flush();
                    thread::sleep(Duration::from_secs(30));
                    return;
                }
                if mode == "stderr" {
                    eprintln!("do-not-print-this-private-diagnostic");
                }
                if mode == "initialize_error" {
                    send_error(&mut stdout, id(&line), "initialization failed");
                    return;
                }
                if mode == "wrong_id" {
                    send_response(
                        &mut stdout,
                        Some(id(&line).unwrap_or(0) + 99),
                        "{\"ready\":true}",
                    );
                    continue;
                }
                if mode == "missing_id" {
                    let _ = writeln!(stdout, "{{\"result\":{{\"ready\":true}}}}");
                    let _ = stdout.flush();
                    continue;
                }
                if mode == "notification_burst" {
                    for _ in 0..64 {
                        let _ =
                            writeln!(stdout, "{{\"method\":\"account/updated\",\"params\":{{}}}}");
                    }
                    send_response(&mut stdout, id(&line), "{\"ready\":true}");
                    continue;
                }
                let _ = writeln!(stdout, "{{\"method\":\"account/updated\",\"params\":{{}}}}");
                send_response(&mut stdout, id(&line), "{\"ready\":true}");
            }
            "initialized" => {
                if stage != 1 {
                    return;
                }
                stage = 2;
            }
            "account/read" => {
                if stage != 2 || !line.contains("\"refreshToken\":false") {
                    send_error(&mut stdout, id(&line), "bad account request");
                    return;
                }
                stage = 3;
                if mode == "account_error" {
                    send_error(&mut stdout, id(&line), "account read failed");
                    return;
                }
                let account = match mode.as_str() {
                    "no_account" => "null",
                    "api_key" => "{\"type\":\"apiKey\"}",
                    "other_account" => "{\"type\":\"gcp\"}",
                    _ => {
                        "{\"type\":\"chatgpt\",\"email\":\"private-codex-account@example.invalid\"}"
                    }
                };
                send_response(
                    &mut stdout,
                    id(&line),
                    &format!("{{\"account\":{account}}}"),
                );
            }
            "account/rateLimits/read" => {
                if stage != 3 {
                    send_error(&mut stdout, id(&line), "bad rate-limits request");
                    return;
                }
                if mode == "rate_limits_error" {
                    send_error(&mut stdout, id(&line), "rate limits read failed");
                    return;
                }
                let result = rate_limits(&mode);
                send_response(&mut stdout, id(&line), result);
                return;
            }
            _ => return,
        }
    }
}

fn id(line: &str) -> Option<u64> {
    let tail = line.split_once("\"id\":")?.1;
    let digits: String = tail.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

fn send_response(stdout: &mut impl Write, id: Option<u64>, result: &str) {
    let Some(id) = id else { return };
    let _ = writeln!(stdout, "{{\"id\":{id},\"result\":{result}}}");
    let _ = stdout.flush();
}

fn send_error(stdout: &mut impl Write, id: Option<u64>, message: &str) {
    let Some(id) = id else { return };
    let _ = writeln!(
        stdout,
        "{{\"id\":{id},\"error\":{{\"code\":-32000,\"message\":\"{message}\"}}}}"
    );
    let _ = stdout.flush();
}

fn rate_limits(mode: &str) -> &'static str {
    match mode {
        "multiple_buckets" => {
            r#"{"rateLimitsByLimitId":{"codex":{"primary":{"usedPercent":42,"windowDurationMins":300,"resetsAt":1800000000},"secondary":{"usedPercent":0,"windowDurationMins":10080,"resetsAt":1801000000},"planType":"future_plan","rateLimitReachedType":"workspace_owner_credits_depleted"},"other-model":{"primary":{"usedPercent":25}}},"rateLimits":{"primary":{"usedPercent":99}}}"#
        }
        "legacy" | "empty_buckets" => {
            r#"{"rateLimitsByLimitId":{},"rateLimits":{"primary":{"usedPercent":9,"windowDurationMins":60,"resetsAt":1800000000}}}"#
        }
        "nullable_fields" => {
            r#"{"rateLimitsByLimitId":{"codex":{"primary":{"usedPercent":null,"windowDurationMins":null,"resetsAt":null},"secondary":{"usedPercent":0,"windowDurationMins":5}},"empty":{"primary":{"usedPercent":null,"windowDurationMins":null,"resetsAt":null}}}}"#
        }
        "no_data" => {
            r#"{"rateLimitsByLimitId":{},"rateLimits":{"primary":{"usedPercent":null,"windowDurationMins":null,"resetsAt":null}}}"#
        }
        "unknown_variants" => {
            r#"{"rateLimits":{"primary":{"usedPercent":12,"planType":"future_plan","rateLimitReachedType":"workspace_member_credits_depleted"}}}"#
        }
        _ => {
            r#"{"rateLimits":{"primary":{"usedPercent":42,"windowDurationMins":300,"resetsAt":1800000000},"secondary":{"usedPercent":7,"windowDurationMins":10080,"resetsAt":1801000000}}}"#
        }
    }
}
