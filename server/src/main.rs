use std::collections::{HashMap, HashSet};
use std::thread::sleep;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use lsp_server::{Connection, ErrorCode, Message, Request, Response};
use lsp_types::{
    notification::{
        DidChangeTextDocument, DidCloseTextDocument, DidOpenTextDocument, Notification,
    },
    request::{CodeActionRequest, Request as RequestTrait},
    CodeAction, CodeActionKind, CodeActionOrCommand, CodeActionParams,
    CodeActionProviderCapability, InitializeParams, Position, Range, ServerCapabilities,
    TextDocumentSyncCapability, TextDocumentSyncKind, TextEdit, Uri, WorkspaceEdit,
};
use reqwest::blocking::Client;
use serde::Deserialize;

const DEFAULT_API_BASE_URL: &str = "https://apione.apibyte.cn/translate";
const DEFAULT_TIMEOUT_MS: u64 = 15_000;
const DEFAULT_DEBOUNCE_MS_WITH_KEY: u64 = 350;
const DEFAULT_DEBOUNCE_MS_WITHOUT_KEY: u64 = 1_000;
const DEFAULT_ERROR_CACHE_TTL_MS: u64 = 2_000;
const NEARBY_LINE_SEARCH_RADIUS: u32 = 1;

fn main() -> Result<()> {
    let (connection, io_threads) = Connection::stdio();

    let server_capabilities = serde_json::to_value(ServerCapabilities {
        code_action_provider: Some(CodeActionProviderCapability::Simple(true)),
        text_document_sync: Some(TextDocumentSyncCapability::Kind(TextDocumentSyncKind::FULL)),
        ..ServerCapabilities::default()
    })?;

    let initialize_value = connection.initialize(server_capabilities)?;
    let config = ServerConfig::from_initialize_value(initialize_value);
    let run_result = run(connection, config);
    io_threads.join()?;
    run_result
}

fn run(connection: Connection, config: ServerConfig) -> Result<()> {
    let timeout = Duration::from_millis(config.timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS));
    let client = Client::builder().timeout(timeout).build()?;

    eprintln!(
        "[translate-plugin] server started api_base_url={} has_api_key={} timeout_ms={} debounce_ms={} error_cache_ttl_ms={}",
        config.api_base_url(),
        config.api_key().is_some(),
        config.timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS),
        config.debounce_window().as_millis(),
        config.error_cache_ttl().as_millis()
    );

    let mut state = ServerState {
        documents: HashMap::new(),
        config,
        client,
        cache: HashMap::new(),
        failure_cache: HashMap::new(),
        last_remote_request_at: None,
    };

    for message in &connection.receiver {
        match message {
            Message::Request(request) => {
                if connection.handle_shutdown(&request)? {
                    return Ok(());
                }

                handle_request(&connection, &mut state, &request)?;
            }
            Message::Notification(notification) => {
                handle_notification(&mut state, &notification)?;
            }
            Message::Response(_) => {}
        }
    }

    Ok(())
}

fn handle_request(
    connection: &Connection,
    state: &mut ServerState,
    request: &Request,
) -> Result<()> {
    match request.method.as_str() {
        CodeActionRequest::METHOD => {
            let params: CodeActionParams = serde_json::from_value(request.params.clone())
                .context("failed to parse code action params")?;
            let actions = build_code_actions(state, &params);
            let result = serde_json::to_value(&actions)?;
            send_response(connection, Response::new_ok(request.id.clone(), result))?;
        }
        _ => {
            send_response(
                connection,
                Response::new_err(
                    request.id.clone(),
                    ErrorCode::MethodNotFound as i32,
                    format!("unsupported request: {}", request.method),
                ),
            )?;
        }
    }

    Ok(())
}

fn handle_notification(
    state: &mut ServerState,
    notification: &lsp_server::Notification,
) -> Result<()> {
    match notification.method.as_str() {
        DidOpenTextDocument::METHOD => {
            let params: lsp_types::DidOpenTextDocumentParams =
                serde_json::from_value(notification.params.clone())
                    .context("failed to parse didOpen notification")?;
            eprintln!(
                "[translate-plugin] document opened uri={:?} chars={}",
                params.text_document.uri,
                params.text_document.text.chars().count()
            );
            state
                .documents
                .insert(params.text_document.uri, params.text_document.text);
        }
        DidChangeTextDocument::METHOD => {
            let params: lsp_types::DidChangeTextDocumentParams =
                serde_json::from_value(notification.params.clone())
                    .context("failed to parse didChange notification")?;

            if let Some(change) = params.content_changes.into_iter().last() {
                eprintln!(
                    "[translate-plugin] document changed uri={:?} chars={}",
                    params.text_document.uri,
                    change.text.chars().count()
                );
                state
                    .documents
                    .insert(params.text_document.uri, change.text);
            }
        }
        DidCloseTextDocument::METHOD => {
            let params: lsp_types::DidCloseTextDocumentParams =
                serde_json::from_value(notification.params.clone())
                    .context("failed to parse didClose notification")?;
            eprintln!(
                "[translate-plugin] document closed uri={:?}",
                params.text_document.uri
            );
            state.documents.remove(&params.text_document.uri);
        }
        _ => {}
    }

    Ok(())
}

fn send_response(connection: &Connection, response: Response) -> Result<()> {
    connection
        .sender
        .send(Message::Response(response))
        .context("failed to send LSP response")
}

fn build_code_actions(
    state: &mut ServerState,
    params: &CodeActionParams,
) -> Option<Vec<CodeActionOrCommand>> {
    let document_uri = &params.text_document.uri;
    eprintln!(
        "[translate-plugin] codeAction requested uri={:?} range={}",
        document_uri,
        format_range(&params.range)
    );

    let Some(document_text) = state.documents.get(document_uri).cloned() else {
        eprintln!(
            "[translate-plugin] codeAction skipped: document not cached for {:?}",
            document_uri
        );
        return None;
    };

    let Some(resolved_target) = resolve_translation_target(&document_text, &params.range) else {
        eprintln!(
            "[translate-plugin] codeAction skipped: failed to resolve target for {:?} at {:?}",
            document_uri, params.range
        );
        return None;
    };
    let target_range = resolved_target.range;
    let source_text = resolved_target.text;
    eprintln!(
        "[translate-plugin] codeAction resolved source={} text={:?} target_range={}",
        resolved_target.source,
        summarize_text(&source_text),
        format_range(&target_range)
    );

    let mut actions = Vec::new();
    let mut seen_replacements = HashSet::new();
    let mut translation_error = None;
    let mut style_error = None;

    match state.translate_auto(&source_text) {
        Ok(translation) => {
            if translation.target_text != source_text
                && seen_replacements.insert(translation.target_text.clone())
            {
                actions.push(make_replace_action(
                    format!("Translate {}", truncate_for_title(&translation.target_text)),
                    document_uri,
                    &target_range,
                    translation.target_text,
                    true,
                ));
            }
        }
        Err(error) => {
            translation_error = Some(error.to_string());
        }
    }

    match state.words_for_identifier_styles(&source_text) {
        Ok(words) => {
            let variants = [
                ("lowercase", to_lower_flat(&words)),
                ("camelCase", to_camel_case(&words)),
                ("PascalCase", to_pascal_case(&words)),
                ("snake_case", to_snake_case(&words)),
            ];

            for (style_name, replacement) in variants {
                if !replacement.is_empty()
                    && replacement != source_text
                    && seen_replacements.insert(replacement.clone())
                {
                    actions.push(make_replace_action(
                        format!("Replace with {} ({})", replacement, style_name),
                        document_uri,
                        &target_range,
                        replacement,
                        false,
                    ));
                }
            }
        }
        Err(error) => {
            style_error = Some(error.to_string());
        }
    }

    eprintln!(
        "[translate-plugin] codeAction target={:?} actions={} translation_error={:?} style_error={:?}",
        source_text,
        actions.len(),
        translation_error,
        style_error
    );

    (!actions.is_empty()).then_some(actions)
}

fn truncate_for_title(text: &str) -> String {
    const LIMIT: usize = 28;
    let mut shortened = String::new();

    for (index, character) in text.chars().enumerate() {
        if index >= LIMIT {
            shortened.push_str("...");
            break;
        }
        shortened.push(character);
    }

    shortened
}

fn make_replace_action(
    title: String,
    document_uri: &Uri,
    range: &Range,
    replacement: String,
    is_preferred: bool,
) -> CodeActionOrCommand {
    CodeActionOrCommand::CodeAction(CodeAction {
        title,
        kind: Some(CodeActionKind::REFACTOR_REWRITE),
        edit: Some(WorkspaceEdit {
            changes: Some(HashMap::from([(
                document_uri.clone(),
                vec![TextEdit {
                    range: range.clone(),
                    new_text: replacement,
                }],
            )])),
            document_changes: None,
            change_annotations: None,
        }),
        is_preferred: Some(is_preferred),
        ..CodeAction::default()
    })
}

fn resolve_translation_target(
    document_text: &str,
    requested_range: &Range,
) -> Option<ResolvedTarget> {
    if requested_range.start != requested_range.end {
        let selected_text = extract_text_in_range(document_text, requested_range)?;
        if contains_meaningful_text(&selected_text) {
            return Some(ResolvedTarget {
                range: requested_range.clone(),
                text: selected_text,
                source: "selection",
            });
        }

        eprintln!(
            "[translate-plugin] selection fallback triggered: range={} extracted={:?}",
            format_range(requested_range),
            summarize_text(&selected_text)
        );
    }

    resolve_token_target(document_text, requested_range.start, "cursor")
        .or_else(|| resolve_token_target(document_text, requested_range.end, "range-end"))
}

fn extract_text_in_range(document_text: &str, range: &Range) -> Option<String> {
    let start = position_to_byte_offset(document_text, range.start)?;
    let end = position_to_byte_offset(document_text, range.end)?;
    document_text.get(start..end).map(ToString::to_string)
}

fn position_to_byte_offset(document_text: &str, position: Position) -> Option<usize> {
    let mut offset = 0usize;
    let mut lines = document_text.split('\n');

    for _ in 0..position.line {
        let line = lines.next()?;
        offset += line.len() + 1;
    }

    let line = lines.next()?;
    Some(offset + utf16_to_byte_offset(line, position.character))
}

fn find_token_span(line: &str, byte_offset: usize) -> Option<(usize, usize)> {
    let spans = token_spans(line);
    if spans.is_empty() {
        return None;
    }

    let preferred_offset = spans
        .iter()
        .any(|&(start, end)| start <= byte_offset && byte_offset < end)
        .then_some(byte_offset)
        .or_else(|| byte_offset.checked_sub(1))?;

    spans
        .into_iter()
        .find(|&(start, end)| start <= preferred_offset && preferred_offset < end)
}

fn find_token_span_near(line: &str, byte_offset: usize) -> Option<(usize, usize)> {
    let spans = token_spans(line);
    if spans.is_empty() {
        return None;
    }

    if let Some(span) = find_token_span(line, byte_offset) {
        return Some(span);
    }

    if let Some(left_offset) = byte_offset.checked_sub(1) {
        if let Some(span) = find_token_span(line, left_offset) {
            return Some(span);
        }
    }

    spans.into_iter().min_by_key(|&(start, end)| {
        if byte_offset <= start {
            start - byte_offset
        } else {
            byte_offset.saturating_sub(end)
        }
    })
}

fn token_spans(line: &str) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    let mut current_start = None;

    for (index, character) in line.char_indices() {
        if is_token_char(character) {
            current_start.get_or_insert(index);
        } else if let Some(start) = current_start.take() {
            spans.push((start, index));
        }
    }

    if let Some(start) = current_start {
        spans.push((start, line.len()));
    }

    spans
}

fn is_token_char(character: char) -> bool {
    character.is_alphanumeric() || matches!(character, '_' | '-')
}

fn contains_meaningful_text(text: &str) -> bool {
    text.chars()
        .any(|character| !character.is_whitespace() && !character.is_control())
}

fn utf16_to_byte_offset(line: &str, utf16_offset: u32) -> usize {
    let mut consumed = 0;

    for (index, character) in line.char_indices() {
        if consumed >= utf16_offset {
            return index;
        }

        let width = character.len_utf16() as u32;
        if consumed + width > utf16_offset {
            return index;
        }

        consumed += width;
    }

    line.len()
}

fn byte_to_utf16_offset(line: &str, byte_offset: usize) -> u32 {
    line[..byte_offset]
        .chars()
        .map(|character| character.len_utf16() as u32)
        .sum()
}

fn utf16_len(line: &str) -> u32 {
    line.chars()
        .map(|character| character.len_utf16() as u32)
        .sum()
}

fn resolve_token_target(
    document_text: &str,
    position: Position,
    source: &'static str,
) -> Option<ResolvedTarget> {
    let lines: Vec<&str> = document_text.split('\n').collect();
    let line_index = position.line as usize;
    let line = *lines.get(line_index)?;

    if let Some(target) =
        resolve_token_target_on_line(line, position.line, position.character, source)
    {
        return Some(target);
    }

    let line_is_stale_or_empty = line.trim().is_empty() || position.character > utf16_len(line);
    if !line_is_stale_or_empty {
        return None;
    }

    for distance in 1..=NEARBY_LINE_SEARCH_RADIUS {
        if let Some(previous_line) = line_index.checked_sub(distance as usize) {
            if let Some(candidate_line) = lines.get(previous_line).copied() {
                if let Some(target) = resolve_token_target_on_line(
                    candidate_line,
                    previous_line as u32,
                    position.character,
                    "nearby-line",
                ) {
                    return Some(target);
                }
            }
        }

        if let Some(candidate_line) = lines.get(line_index + distance as usize).copied() {
            if let Some(target) = resolve_token_target_on_line(
                candidate_line,
                (line_index + distance as usize) as u32,
                position.character,
                "nearby-line",
            ) {
                return Some(target);
            }
        }
    }

    None
}

fn resolve_token_target_on_line(
    line: &str,
    line_number: u32,
    requested_character: u32,
    source: &'static str,
) -> Option<ResolvedTarget> {
    let byte_offset = utf16_to_byte_offset(line, requested_character);
    let (start_byte, end_byte) = find_token_span_near(line, byte_offset)?;
    let target_range = Range {
        start: Position {
            line: line_number,
            character: byte_to_utf16_offset(line, start_byte),
        },
        end: Position {
            line: line_number,
            character: byte_to_utf16_offset(line, end_byte),
        },
    };

    Some(ResolvedTarget {
        range: target_range,
        text: line[start_byte..end_byte].to_string(),
        source,
    })
}

fn format_range(range: &Range) -> String {
    format!(
        "{}:{}-{}:{}",
        range.start.line, range.start.character, range.end.line, range.end.character
    )
}

fn summarize_text(text: &str) -> String {
    const LIMIT: usize = 40;
    let normalized = text.replace('\n', "\\n");
    let mut shortened = String::new();

    for (index, character) in normalized.chars().enumerate() {
        if index >= LIMIT {
            shortened.push_str("...");
            return shortened;
        }
        shortened.push(character);
    }

    shortened
}

fn split_identifier(input: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let characters: Vec<char> = input.chars().collect();

    for (index, character) in characters.iter().copied().enumerate() {
        if matches!(character, '_' | '-' | '/' | '\\' | '.') {
            flush_current(&mut current, &mut parts);
            continue;
        }

        if !character.is_alphanumeric() {
            flush_current(&mut current, &mut parts);
            continue;
        }

        let starts_new_word = if let Some(previous) = characters.get(index.saturating_sub(1)) {
            (previous.is_ascii_lowercase() && character.is_ascii_uppercase())
                || (previous.is_ascii_alphabetic() && character.is_ascii_digit())
                || (previous.is_ascii_digit() && character.is_ascii_alphabetic())
        } else {
            false
        };

        if starts_new_word {
            flush_current(&mut current, &mut parts);
        }

        current.push(character);
    }

    flush_current(&mut current, &mut parts);
    parts
}

fn flush_current(current: &mut String, parts: &mut Vec<String>) {
    if !current.is_empty() {
        parts.push(current.to_ascii_lowercase());
        current.clear();
    }
}

fn contains_cjk(input: &str) -> bool {
    input.chars().any(|character| {
        matches!(
            character as u32,
            0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF
        )
    })
}

fn has_identifier_structure(input: &str) -> bool {
    input.contains(['_', '-', ' ', '.'])
        || input
            .chars()
            .any(|character| character.is_ascii_uppercase())
}

fn normalize_english_words(input: &str) -> Vec<String> {
    split_identifier(input)
        .into_iter()
        .filter(|piece| {
            piece
                .chars()
                .any(|character| character.is_ascii_alphanumeric())
        })
        .collect()
}

fn to_lower_flat(words: &[String]) -> String {
    words.join("")
}

fn to_camel_case(words: &[String]) -> String {
    let mut output = String::new();

    for (index, word) in words.iter().enumerate() {
        if index == 0 {
            output.push_str(word);
        } else {
            output.push_str(&capitalize_ascii(word));
        }
    }

    output
}

fn to_pascal_case(words: &[String]) -> String {
    words
        .iter()
        .map(|word| capitalize_ascii(word))
        .collect::<String>()
}

fn to_snake_case(words: &[String]) -> String {
    words.join("_")
}

fn capitalize_ascii(word: &str) -> String {
    let mut characters = word.chars();
    match characters.next() {
        Some(first) => first.to_ascii_uppercase().to_string() + characters.as_str(),
        None => String::new(),
    }
}

fn normalize_api_key(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }

    let normalized = trimmed
        .strip_prefix("Bearer ")
        .or_else(|| trimmed.strip_prefix("bearer "))
        .unwrap_or(trimmed)
        .trim();

    (!normalized.is_empty()).then(|| normalized.to_string())
}

#[derive(Default, Clone, Deserialize)]
struct ServerConfig {
    #[serde(
        alias = "key",
        alias = "apiKey",
        alias = "token",
        alias = "bearer_token"
    )]
    api_key: Option<String>,
    #[serde(alias = "apiBaseUrl", alias = "base_url")]
    api_base_url: Option<String>,
    timeout_ms: Option<u64>,
    debounce_ms: Option<u64>,
    error_cache_ttl_ms: Option<u64>,
}

impl ServerConfig {
    fn from_initialize_value(value: serde_json::Value) -> Self {
        serde_json::from_value::<InitializeParams>(value)
            .ok()
            .and_then(|params| params.initialization_options)
            .and_then(|options| serde_json::from_value(options).ok())
            .unwrap_or_default()
    }

    fn api_base_url(&self) -> &str {
        self.api_base_url
            .as_deref()
            .filter(|value| !value.is_empty())
            .unwrap_or(DEFAULT_API_BASE_URL)
    }

    fn api_key(&self) -> Option<String> {
        self.api_key.as_deref().and_then(normalize_api_key)
    }

    fn debounce_window(&self) -> Duration {
        let debounce_ms = self.debounce_ms.unwrap_or_else(|| {
            if self.api_key().is_some() {
                DEFAULT_DEBOUNCE_MS_WITH_KEY
            } else {
                DEFAULT_DEBOUNCE_MS_WITHOUT_KEY
            }
        });

        Duration::from_millis(debounce_ms)
    }

    fn error_cache_ttl(&self) -> Duration {
        Duration::from_millis(
            self.error_cache_ttl_ms
                .unwrap_or(DEFAULT_ERROR_CACHE_TTL_MS),
        )
    }
}

struct ServerState {
    documents: HashMap<Uri, String>,
    config: ServerConfig,
    client: Client,
    cache: HashMap<TranslationCacheKey, TranslationOutcome>,
    failure_cache: HashMap<TranslationCacheKey, CachedFailure>,
    last_remote_request_at: Option<Instant>,
}

impl ServerState {
    fn translate_auto(&mut self, text: &str) -> Result<TranslationOutcome> {
        self.translate_to(text, auto_target_language(text))
    }

    fn words_for_identifier_styles(&mut self, source_text: &str) -> Result<Vec<String>> {
        if contains_cjk(source_text) {
            let translation = self.translate_to(source_text, TargetLanguage::English)?;
            let words = normalize_english_words(&translation.target_text);
            if words.is_empty() {
                return Err(anyhow!("translation returned no English words"));
            }

            return Ok(words);
        }

        let direct_words = normalize_english_words(source_text);
        if direct_words.len() > 1 || has_identifier_structure(source_text) {
            return Ok(direct_words);
        }

        if direct_words.is_empty() {
            return Err(anyhow!("selection does not contain identifier text"));
        }

        let chinese = self.translate_to(source_text, TargetLanguage::Chinese)?;
        let english = self.translate_to(&chinese.target_text, TargetLanguage::English)?;
        let roundtrip_words = normalize_english_words(&english.target_text);

        if roundtrip_words.is_empty() {
            Ok(direct_words)
        } else {
            Ok(roundtrip_words)
        }
    }

    fn translate_to(
        &mut self,
        text: &str,
        target_language: TargetLanguage,
    ) -> Result<TranslationOutcome> {
        let cache_key = TranslationCacheKey {
            text: text.to_string(),
            target_language,
        };

        if let Some(cached) = self.cache.get(&cache_key) {
            eprintln!(
                "[translate-plugin] translate cache hit to={} text={:?}",
                target_language.code(),
                summarize_text(text)
            );
            return Ok(cached.clone());
        }

        self.prune_failure_cache();

        if let Some(cached_failure) = self.failure_cache.get(&cache_key) {
            return Err(anyhow!(cached_failure.message.clone()));
        }

        self.wait_for_request_slot();
        let api_key = self.config.api_key();
        eprintln!(
            "[translate-plugin] translate request to={} has_api_key={} text={:?}",
            target_language.code(),
            api_key.is_some(),
            summarize_text(text)
        );

        let mut request = self.client.get(self.config.api_base_url()).query(&[
            ("text", text),
            ("from", "auto"),
            ("to", target_language.code()),
        ]);

        if let Some(api_key) = api_key {
            request = request.query(&[("key", api_key.as_str())]);
        }

        let response = request
            .send()
            .with_context(|| format!("failed to request translation for {text:?}"));

        let response = match response {
            Ok(response) => response,
            Err(error) => {
                eprintln!(
                    "[translate-plugin] translate request failed to={} text={:?} error={}",
                    target_language.code(),
                    summarize_text(text),
                    error
                );
                self.cache_failure(&cache_key, error.to_string());
                return Err(error);
            }
        };

        let status = response.status();
        let payload = response
            .json()
            .context("failed to decode translation response");

        let payload: TranslateApiResponse = match payload {
            Ok(payload) => payload,
            Err(error) => {
                eprintln!(
                    "[translate-plugin] translate decode failed to={} text={:?} error={}",
                    target_language.code(),
                    summarize_text(text),
                    error
                );
                self.cache_failure(&cache_key, error.to_string());
                return Err(error);
            }
        };

        if !status.is_success() {
            let message = format!(
                "translation API returned HTTP {}: {}",
                status,
                payload.message()
            );
            eprintln!(
                "[translate-plugin] translate http failure to={} text={:?} error={}",
                target_language.code(),
                summarize_text(text),
                message
            );
            self.cache_failure(&cache_key, message.clone());
            return Err(anyhow!(message));
        }

        if !payload.is_success() {
            let message = payload.message();
            eprintln!(
                "[translate-plugin] translate api failure to={} text={:?} error={}",
                target_language.code(),
                summarize_text(text),
                message
            );
            self.cache_failure(&cache_key, message.clone());
            return Err(anyhow!(message));
        }

        let data = payload
            .data
            .ok_or_else(|| anyhow!("translation API returned no data"))?;

        let outcome = TranslationOutcome {
            target_text: data
                .target_text
                .filter(|value| !value.is_empty())
                .ok_or_else(|| anyhow!("translation API returned empty target text"))?,
        };
        eprintln!(
            "[translate-plugin] translate success to={} source={:?} result={:?}",
            target_language.code(),
            summarize_text(text),
            summarize_text(&outcome.target_text)
        );

        self.failure_cache.remove(&cache_key);
        self.cache.insert(cache_key, outcome.clone());
        Ok(outcome)
    }

    fn wait_for_request_slot(&mut self) {
        let debounce_window = self.config.debounce_window();

        if let Some(last_remote_request_at) = self.last_remote_request_at {
            let elapsed = last_remote_request_at.elapsed();
            if elapsed < debounce_window {
                sleep(debounce_window - elapsed);
            }
        }

        self.last_remote_request_at = Some(Instant::now());
    }

    fn cache_failure(&mut self, cache_key: &TranslationCacheKey, message: String) {
        self.failure_cache.insert(
            cache_key.clone(),
            CachedFailure {
                message,
                created_at: Instant::now(),
            },
        );
    }

    fn prune_failure_cache(&mut self) {
        let error_cache_ttl = self.config.error_cache_ttl();
        self.failure_cache
            .retain(|_, failure| failure.created_at.elapsed() < error_cache_ttl);
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct TranslationCacheKey {
    text: String,
    target_language: TargetLanguage,
}

#[derive(Clone, Debug)]
struct CachedFailure {
    message: String,
    created_at: Instant,
}

#[derive(Clone, Debug)]
struct ResolvedTarget {
    range: Range,
    text: String,
    source: &'static str,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum TargetLanguage {
    Chinese,
    English,
}

impl TargetLanguage {
    fn code(self) -> &'static str {
        match self {
            Self::Chinese => "zh",
            Self::English => "en",
        }
    }
}

fn auto_target_language(text: &str) -> TargetLanguage {
    if contains_cjk(text) {
        TargetLanguage::English
    } else {
        TargetLanguage::Chinese
    }
}

#[derive(Clone, Debug)]
struct TranslationOutcome {
    target_text: String,
}

#[derive(Debug, Deserialize)]
struct TranslateApiResponse {
    code: Option<i64>,
    msg: Option<String>,
    message: Option<String>,
    data: Option<TranslateApiData>,
}

impl TranslateApiResponse {
    fn is_success(&self) -> bool {
        matches!(self.code, Some(0) | Some(200) | None)
    }

    fn message(&self) -> String {
        self.message
            .clone()
            .or_else(|| self.msg.clone())
            .unwrap_or_else(|| "translation request failed".to_string())
    }
}

#[derive(Clone, Debug, Deserialize)]
struct TranslateApiData {
    target_text: Option<String>,
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use lsp_types::{Position, Range};

    use super::{
        auto_target_language, contains_cjk, extract_text_in_range, find_token_span,
        find_token_span_near, normalize_api_key, normalize_english_words,
        resolve_translation_target, split_identifier, to_camel_case, to_lower_flat, to_pascal_case,
        to_snake_case, truncate_for_title, ServerConfig, ServerState, TargetLanguage,
        TranslationCacheKey, TranslationOutcome, DEFAULT_DEBOUNCE_MS_WITHOUT_KEY,
        DEFAULT_DEBOUNCE_MS_WITH_KEY,
    };

    #[test]
    fn splits_snake_and_camel_case_identifiers() {
        assert_eq!(
            split_identifier("hoverTranslate_server"),
            vec!["hover", "translate", "server"]
        );
    }

    #[test]
    fn normalizes_identifier_words() {
        assert_eq!(
            normalize_english_words("UserName and user_name"),
            vec!["user", "name", "and", "user", "name"]
        );
    }

    #[test]
    fn builds_case_variants() {
        let words = vec!["user".to_string(), "name".to_string()];
        assert_eq!(to_lower_flat(&words), "username");
        assert_eq!(to_camel_case(&words), "userName");
        assert_eq!(to_pascal_case(&words), "UserName");
        assert_eq!(to_snake_case(&words), "user_name");
    }

    #[test]
    fn detects_translation_direction() {
        assert_eq!(auto_target_language("hello_world"), TargetLanguage::Chinese);
        assert_eq!(auto_target_language("用户名"), TargetLanguage::English);
        assert!(contains_cjk("你好"));
    }

    #[test]
    fn uses_key_aware_default_debounce() {
        assert_eq!(
            ServerConfig::default().debounce_window(),
            std::time::Duration::from_millis(DEFAULT_DEBOUNCE_MS_WITHOUT_KEY)
        );

        assert_eq!(
            ServerConfig {
                api_key: Some("demo-key".to_string()),
                ..ServerConfig::default()
            }
            .debounce_window(),
            std::time::Duration::from_millis(DEFAULT_DEBOUNCE_MS_WITH_KEY)
        );
    }

    #[test]
    fn extracts_selected_text_across_one_line() {
        let range = Range {
            start: Position {
                line: 0,
                character: 4,
            },
            end: Position {
                line: 0,
                character: 14,
            },
        };

        assert_eq!(
            extract_text_in_range("let request_id = 1;", &range).as_deref(),
            Some("request_id")
        );
    }

    #[test]
    fn finds_span_around_hovered_word() {
        assert_eq!(find_token_span("let request_id = 1;", 5), Some((4, 14)));
    }

    #[test]
    fn finds_nearest_span_when_cursor_is_past_line_end() {
        assert_eq!(
            find_token_span_near("            .json()", 19),
            Some((13, 17))
        );
    }

    #[test]
    fn resolves_cursor_to_nearest_token_even_with_stale_column() {
        let range = Range {
            start: Position {
                line: 0,
                character: 51,
            },
            end: Position {
                line: 0,
                character: 51,
            },
        };

        let resolved = resolve_translation_target("            .json()", &range).expect("target");
        assert_eq!(resolved.text, "json");
        assert_eq!(resolved.range.start.character, 13);
        assert_eq!(resolved.range.end.character, 17);
    }

    #[test]
    fn falls_back_from_blank_line_to_nearby_token() {
        let document = "let status = response.status();\n\nlet payload = response.json();";
        let range = Range {
            start: Position {
                line: 1,
                character: 59,
            },
            end: Position {
                line: 1,
                character: 59,
            },
        };

        let resolved = resolve_translation_target(document, &range).expect("target");
        assert_eq!(resolved.text, "status");
        assert_eq!(resolved.range.start.line, 0);
    }

    #[test]
    fn truncates_long_translation_titles() {
        assert_eq!(
            truncate_for_title(
                "This translation title is definitely longer than twenty eight characters"
            ),
            "This translation title is de..."
        );
    }

    #[test]
    fn normalizes_api_key_input() {
        assert_eq!(
            normalize_api_key("  Bearer demo-key  ").as_deref(),
            Some("demo-key")
        );
        assert_eq!(normalize_api_key("   "), None);
    }

    #[test]
    fn deserializes_key_aliases() {
        let config: ServerConfig = serde_json::from_value(serde_json::json!({
            "key": "demo-key"
        }))
        .expect("server config");

        assert_eq!(config.api_key().as_deref(), Some("demo-key"));
    }

    #[test]
    fn translate_auto_reads_cached_translation_without_request() {
        let mut state = ServerState {
            documents: HashMap::new(),
            config: ServerConfig::default(),
            client: reqwest::blocking::Client::builder()
                .build()
                .expect("client"),
            cache: HashMap::from([(
                TranslationCacheKey {
                    text: "hello".to_string(),
                    target_language: TargetLanguage::Chinese,
                },
                TranslationOutcome {
                    target_text: "你好".to_string(),
                },
            )]),
            failure_cache: HashMap::new(),
            last_remote_request_at: None,
        };

        assert_eq!(
            state
                .translate_auto("hello")
                .ok()
                .map(|item| item.target_text),
            Some("你好".to_string())
        );
    }
}
