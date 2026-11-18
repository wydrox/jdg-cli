use crate::*;

const ANTHROPIC_MESSAGES_URL: &str = "https://api.anthropic.com/v1/messages";
/// Current Messages API version used by the PDF and structured-output examples.
pub(crate) const ANTHROPIC_VERSION: &str = "2026-10-21";
pub(crate) const DEFAULT_ANTHROPIC_MODEL: &str = "claude-haiku-5-5";

pub(crate) fn anthropic_configured() -> bool {
    secret_is_set(Secret::AnthropicApiKey)
}

pub(crate) fn anthropic_model_from(configured: Option<&str>) -> String {
    configured
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(DEFAULT_ANTHROPIC_MODEL)
        .to_string()
}

pub(crate) fn anthropic_model() -> String {
    anthropic_model_from(jdg_config_var("JDG_ANTHROPIC_MODEL").as_deref())
}

pub(crate) fn anthropic_progress_line(processed: usize, todo: usize, filename: &str) -> String {
    llm_progress_line("Anthropic", processed, todo, filename)
}

fn anthropic_timeout() -> Duration {
    Duration::from_secs(
        jdg_config_var("JDG_ANTHROPIC_TIMEOUT_SECS")
            .and_then(|value| value.parse().ok())
            .unwrap_or(120)
            .clamp(15, 300),
    )
}

/// Models after Claude Opus 4.6 reject every temperature except 1.0, so the
/// request omits it and structured output pins the JSON instead.
pub(crate) fn anthropic_invoice_request(model: &str, pdf_base64: &str) -> Value {
    serde_json::json!({
        "model": model,
        "max_tokens": 4000,
        "messages": [{
            "role": "user",
            "content": [
                {
                    "type": "document",
                    "source": {
                        "type": "base64",
                        "media_type": "application/pdf",
                        "data": pdf_base64
                    },
                    "citations": { "enabled": false }
                },
                {
                    "type": "text",
                    "text": INVOICE_PDF_EXTRACTION_PROMPT
                }
            ]
        }],
        "output_config": {
            "format": {
                "type": "json_schema",
                "schema": invoice_structured_schema()
            }
        }
    })
}

pub(crate) fn redact_secret(text: &str, secret: &str) -> String {
    if secret.len() < 8 {
        return text.to_string();
    }
    text.replace(secret, "********")
}

pub(crate) fn anthropic_http_error(status: u16, body: &str, api_key: &str) -> anyhow::Error {
    let message = redact_secret(&format!("Anthropic HTTP {status}: {body}"), api_key);
    if http_status_is_transient(status) {
        transient_llm_error(message)
    } else {
        anyhow!(message)
    }
}

fn anthropic_error_is_transient(error_type: &str) -> bool {
    matches!(
        error_type,
        "overloaded_error" | "rate_limit_error" | "api_error" | "timeout_error"
    )
}

pub(crate) fn anthropic_response_json(response: &Value) -> Result<Value> {
    if response.get("type").and_then(Value::as_str) == Some("error") {
        let error = response.get("error");
        let message = error
            .and_then(|item| item.get("message"))
            .and_then(Value::as_str)
            .unwrap_or("błąd Anthropic");
        let kind = error
            .and_then(|item| item.get("type"))
            .and_then(Value::as_str)
            .unwrap_or("");
        if anthropic_error_is_transient(kind) {
            return Err(transient_llm_error(format!("Anthropic: {message}")));
        }
        return Err(anyhow!("Anthropic: {message}"));
    }
    if response
        .get("stop_reason")
        .and_then(Value::as_str)
        .is_none_or(|reason| reason != "end_turn")
    {
        return Err(anyhow!(
            "Anthropic: odpowiedź nie została zakończona poprawnie; nie zapisuję częściowego JSON"
        ));
    }
    let content = response
        .get("content")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("Anthropic: brak treści odpowiedzi"))?;
    let mut text = String::new();
    for block in content {
        if block.get("type").and_then(Value::as_str) != Some("text") {
            continue;
        }
        if let Some(piece) = block.get("text").and_then(Value::as_str) {
            text.push_str(piece);
        }
    }
    if text.is_empty() {
        return Err(anyhow!("Anthropic: brak treści odpowiedzi"));
    }
    crate::parse_json_from_llm(&text)
}

pub(crate) fn anthropic_extract_invoice_fields(
    record: &mut InvoiceRecord,
    path: &Path,
    usage: &mut LlmTokenUsage,
) -> Result<bool> {
    let pdf = load_paid_invoice_pdf(record, path, "Anthropic")?;
    // Nothing is sent without a key, so these failures must not block a later try.
    let api_key = secret_value(Secret::AnthropicApiKey)
        .map_err(|err| transient_llm_error(format!("odczyt klucza Anthropic: {err:#}")))?
        .ok_or_else(|| {
            transient_llm_error(
                "brak ANTHROPIC_API_KEY; ustaw klucz Anthropic albo użyj OpenRouter albo lokalnego LLM",
            )
        })?;
    let model = anthropic_model();
    let body = anthropic_invoice_request(&model, &pdf.pdf_base64);
    let client = Client::builder().timeout(anthropic_timeout()).build()?;
    let response = llm_send_with_retry(
        "Anthropic",
        || match client
            .post(ANTHROPIC_MESSAGES_URL)
            .header("x-api-key", &api_key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .json(&body)
            .send()
        {
            Ok(resp) if resp.status().is_success() => resp
                .json::<Value>()
                .map_err(|err| (OpenRouterFailure::Other, anyhow::Error::from(err))),
            Ok(resp) => {
                let status = resp.status();
                let text = resp.text().unwrap_or_default();
                Err((
                    OpenRouterFailure::Status(status.as_u16()),
                    anthropic_http_error(status.as_u16(), &text, &api_key),
                ))
            }
            Err(err) => Err((openrouter_failure_kind(&err), anyhow::Error::from(err))),
        },
        std::thread::sleep,
    )?;
    *usage = response_token_usage(&response);
    let value = anthropic_response_json(&response)?;
    invoice_validation::apply_normalized_invoice_json(record, &value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_sends_the_pdf_and_the_same_invoice_schema() {
        let body = anthropic_invoice_request(DEFAULT_ANTHROPIC_MODEL, "QQ==");
        assert_eq!(body["model"], DEFAULT_ANTHROPIC_MODEL);
        assert_eq!(body["max_tokens"], 4000);
        assert!(body.get("temperature").is_none());
        assert_eq!(body["output_config"]["format"]["type"], "json_schema");
        assert_eq!(
            body["output_config"]["format"]["schema"],
            invoice_structured_schema()
        );
        let content = body["messages"][0]["content"].as_array().unwrap();
        assert_eq!(content[0]["type"], "document");
        assert_eq!(content[0]["source"]["type"], "base64");
        assert_eq!(content[0]["source"]["media_type"], "application/pdf");
        assert_eq!(content[0]["source"]["data"], "QQ==");
        assert_eq!(content[0]["citations"]["enabled"], false);
        assert_eq!(content[1]["type"], "text");
        assert_eq!(content[1]["text"], INVOICE_PDF_EXTRACTION_PROMPT);
        let openrouter = openrouter_invoice_request("m", "f.pdf", "QQ==");
        assert_eq!(
            openrouter["messages"][0]["content"][0]["text"],
            content[1]["text"]
        );
        let encoded = serde_json::to_string(&body).unwrap();
        assert!(!encoded.contains("x-api-key"));
        assert!(!encoded.contains("ANTHROPIC"));
        assert!(!encoded.contains("sk-ant"));
        assert!(!encoded.contains("Bearer"));
        assert!(!encoded.contains("Tekst PDF"));
        assert!(encoded.contains("application/pdf"));
    }

    #[test]
    fn model_override_trims_and_falls_back() {
        assert_eq!(anthropic_model_from(None), DEFAULT_ANTHROPIC_MODEL);
        assert_eq!(anthropic_model_from(Some("  ")), DEFAULT_ANTHROPIC_MODEL);
        assert_eq!(
            anthropic_model_from(Some(" claude-haiku-5-5 ")),
            "claude-haiku-5-5"
        );
    }

    #[test]
    fn progress_line_matches_paid_provider_style() {
        assert_eq!(
            anthropic_progress_line(2, 8, "Invoice.pdf"),
            "Anthropic 2/8 (25%) Invoice.pdf"
        );
    }

    fn invoice_message(text: &str, stop_reason: &str) -> Value {
        serde_json::json!({
            "type": "message",
            "stop_reason": stop_reason,
            "content": [
                {"type": "thinking", "thinking": "nie zapisuj"},
                {"type": "text", "text": text}
            ],
            "usage": {"input_tokens": 1500, "output_tokens": 42}
        })
    }

    #[test]
    fn complete_json_is_accepted_and_usage_is_separate_from_parsing() {
        let ok = invoice_message("{\"seller_name\":\"Elocity\"}", "end_turn");
        assert_eq!(
            anthropic_response_json(&ok).unwrap()["seller_name"],
            "Elocity"
        );
        assert_eq!(
            response_token_usage(&ok),
            LlmTokenUsage {
                input_tokens: Some(1500),
                output_tokens: Some(42)
            }
        );
        let truncated = invoice_message("{\"seller_name\":\"Elocity\"}", "max_tokens");
        assert!(anthropic_response_json(&truncated).is_err());
        assert_eq!(response_token_usage(&truncated).output_tokens, Some(42));
        let refusal = invoice_message("nie", "refusal");
        let err = anthropic_response_json(&refusal).unwrap_err();
        assert!(!llm_error_is_transient(&err));
        assert!(anthropic_response_json(&serde_json::json!({"content":[]})).is_err());
    }

    #[test]
    fn api_errors_are_classified_and_do_not_echo_the_key() {
        let overloaded = serde_json::json!({
            "type": "error",
            "error": {"type": "overloaded_error", "message": "Overloaded"}
        });
        assert!(llm_error_is_transient(
            &anthropic_response_json(&overloaded).unwrap_err()
        ));
        let auth = serde_json::json!({
            "type": "error",
            "error": {"type": "authentication_error", "message": "invalid x-api-key"}
        });
        let err = anthropic_response_json(&auth).unwrap_err().to_string();
        assert!(err.contains("invalid x-api-key"));
        assert!(!err.contains("sk-ant"));
        assert!(!llm_error_is_transient(
            &anthropic_response_json(&auth).unwrap_err()
        ));

        let key = "sk-ant-usr-secret-value";
        let http = anthropic_http_error(529, &format!("overloaded {key}"), key);
        assert!(llm_error_is_transient(&http));
        assert!(http.to_string().contains("529"));
        assert!(!http.to_string().contains(key));
        let denied = anthropic_http_error(401, &format!("bad {key}"), key);
        assert!(!llm_error_is_transient(&denied));
        assert!(!denied.to_string().contains(key));
        assert!(denied.to_string().contains("********"));
    }

    #[test]
    fn client_sends_the_key_only_as_a_header() {
        let source = include_str!("anthropic.rs");
        let production = source.split("#[cfg(test)]").next().unwrap();
        assert!(production.contains(".header(\"x-api-key\""));
        assert!(production.contains("anthropic-version"));
        assert!(production.contains("Secret::AnthropicApiKey"));
        assert!(!production.contains("Authorization"));
        assert!(!production.contains("Bearer "));
        assert!(!production.contains("Command::"));
        assert!(!production.contains("eprintln!"));
        assert!(!production.contains("std::env::var(\"ANTHROPIC_API_KEY\")"));
    }
}
