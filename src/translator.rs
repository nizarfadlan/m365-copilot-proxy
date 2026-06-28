use crate::models::{
    AnthropicMessagesRequest, MessageContent, OpenAIChatRequest, OpenAIResponsesRequest,
    ResponsesInput, TranslatedRequest,
};

pub fn flatten_content(content: &MessageContent) -> String {
    match content {
        MessageContent::Text(s) => s.clone(),
        MessageContent::Parts(parts) => parts
            .iter()
            .filter(|p| p.part_type == "text")
            .filter_map(|p| p.text.as_deref())
            .collect(),
    }
}

pub fn flatten_optional_content(content: Option<&MessageContent>) -> String {
    content.map(flatten_content).unwrap_or_default()
}

fn join_lines(lines: &[String]) -> String {
    lines
        .iter()
        .filter(|l| !l.is_empty())
        .cloned()
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

pub fn translate_openai_request(request: &OpenAIChatRequest) -> Result<TranslatedRequest, String> {
    let mut system_lines = Vec::new();
    let mut transcript_lines = Vec::new();
    let mut prompt = String::new();
    let msg_count = request.messages.len();

    for (index, message) in request.messages.iter().enumerate() {
        let text = flatten_content(&message.content).trim().to_string();
        if text.is_empty() {
            continue;
        }
        let is_last = index == msg_count - 1;
        if message.role == "system" || message.role == "developer" {
            system_lines.push(text);
            continue;
        }
        if is_last {
            if message.role != "user" {
                return Err("The final OpenAI message must be a user message.".into());
            }
            prompt = text;
            continue;
        }
        let role = capitalize_role(&message.role);
        transcript_lines.push(format!("{role}: {text}"));
    }

    if prompt.is_empty() {
        return Err("A final user message is required.".into());
    }

    let mut additional_context = Vec::new();
    let system_text = join_lines(&system_lines);
    if !system_text.is_empty() {
        additional_context.push(format!("System instructions:\n{system_text}"));
    }
    let transcript_text = join_lines(&transcript_lines);
    if !transcript_text.is_empty() {
        additional_context.push(format!("Prior conversation transcript:\n{transcript_text}"));
    }

    Ok(TranslatedRequest {
        prompt,
        additional_context,
    })
}

pub fn translate_responses_request(
    request: &OpenAIResponsesRequest,
) -> Result<TranslatedRequest, String> {
    let instructions = request.instructions.as_deref().unwrap_or("");

    match &request.input {
        ResponsesInput::Text(text) => Ok(TranslatedRequest {
            prompt: text.clone(),
            additional_context: if instructions.is_empty() {
                vec![]
            } else {
                vec![format!("System instructions:\n{instructions}")]
            },
        }),
        ResponsesInput::Messages(items) => {
            let mut system_lines = Vec::new();
            if !instructions.is_empty() {
                system_lines.push(instructions.to_string());
            }
            let mut transcript_lines = Vec::new();
            let mut prompt = String::new();
            let item_count = items.len();

            for (index, item) in items.iter().enumerate() {
                let role = item.get("role").and_then(|v| v.as_str()).unwrap_or("");
                let content = extract_item_content(item);
                let text = content.trim().to_string();
                if text.is_empty() {
                    continue;
                }
                let is_last = index == item_count - 1;
                if role == "system" || role == "developer" {
                    system_lines.push(text);
                    continue;
                }
                if is_last {
                    if role != "user" {
                        return Err(
                            "The final Responses input message must be a user message.".into(),
                        );
                    }
                    prompt = text;
                    continue;
                }
                let role_label = capitalize_role(role);
                transcript_lines.push(format!("{role_label}: {text}"));
            }

            if prompt.is_empty() {
                return Err("No user message found in input.".into());
            }

            let mut additional_context = Vec::new();
            let system_text = join_lines(&system_lines);
            if !system_text.is_empty() {
                additional_context.push(format!("System instructions:\n{system_text}"));
            }
            let transcript_text = join_lines(&transcript_lines);
            if !transcript_text.is_empty() {
                additional_context.push(format!(
                    "Prior conversation transcript:\n{transcript_text}"
                ));
            }

            Ok(TranslatedRequest {
                prompt,
                additional_context,
            })
        }
    }
}

pub fn translate_anthropic_request(
    request: &AnthropicMessagesRequest,
) -> Result<TranslatedRequest, String> {
    let system_text = flatten_optional_content(request.system.as_ref())
        .trim()
        .to_string();
    let mut transcript_lines = Vec::new();
    let mut prompt = String::new();
    let msg_count = request.messages.len();

    for (index, message) in request.messages.iter().enumerate() {
        let text = flatten_content(&message.content).trim().to_string();
        if text.is_empty() {
            continue;
        }
        let is_last = index == msg_count - 1;
        if is_last {
            if message.role != "user" {
                return Err("The final Anthropic message must be a user message.".into());
            }
            prompt = text;
            continue;
        }
        let role = capitalize_role(&message.role);
        transcript_lines.push(format!("{role}: {text}"));
    }

    if prompt.is_empty() {
        return Err("A final user message is required.".into());
    }

    let mut additional_context = Vec::new();
    if !system_text.is_empty() {
        additional_context.push(format!("System instructions:\n{system_text}"));
    }
    let transcript_text = join_lines(&transcript_lines);
    if !transcript_text.is_empty() {
        additional_context.push(format!("Prior conversation transcript:\n{transcript_text}"));
    }

    Ok(TranslatedRequest {
        prompt,
        additional_context,
    })
}

fn capitalize_role(role: &str) -> String {
    let mut chars = role.chars();
    match chars.next() {
        None => String::new(),
        Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
    }
}

fn extract_item_content(item: &serde_json::Value) -> String {
    match item.get("content") {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Array(parts)) => parts
            .iter()
            .filter_map(|p| {
                let part_type = p.get("type")?.as_str()?;
                if part_type == "text" || part_type == "input_text" {
                    p.get("text")?.as_str().map(str::to_string)
                } else {
                    None
                }
            })
            .collect(),
        _ => item.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::OpenAIMessage;

    #[test]
    fn translates_openai_history() {
        let request = OpenAIChatRequest {
            model: "ignored".into(),
            messages: vec![
                OpenAIMessage {
                    role: "system".into(),
                    content: MessageContent::Text("Be concise.".into()),
                },
                OpenAIMessage {
                    role: "user".into(),
                    content: MessageContent::Text("First question".into()),
                },
                OpenAIMessage {
                    role: "assistant".into(),
                    content: MessageContent::Text("First answer".into()),
                },
                OpenAIMessage {
                    role: "user".into(),
                    content: MessageContent::Text("Second question".into()),
                },
            ],
            stream: false,
            temperature: None,
            user: None,
        };

        let translated = translate_openai_request(&request).unwrap();
        assert_eq!(translated.prompt, "Second question");
        assert_eq!(translated.additional_context.len(), 2);
        assert!(translated.additional_context[0].contains("Be concise."));
        assert!(translated.additional_context[1].contains("First question"));
    }
}
